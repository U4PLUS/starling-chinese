//! LittleSkin's skin system (littleskin.cn): the skins players without a Mojang account keep there
//! under their name, which offline launchers and servers show. Asked by name without signing in,
//! each name once a session on a thread of its own, the answers kept on disk so the next start
//! shows them at once while it asks again; Skin Customization's LittleSkin Skins turns it off
//! (`littleskin:off` in horizon-player.txt).
use crate::{offline_name, skins::Textures};
use base64::Engine as _;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// `POST /profiles/minecraft` (body `[{"name": ...}]`): the name's profile, whose `textures`
/// property carries the skin base64-encoded; an empty answer for a name without a profile.
const PROFILES: &str = "https://littleskin.cn/api/yggdrasil/profiles/minecraft";
const KEY: &str = "littleskin";

/// Whether LittleSkin's skins show: on unless Skin Customization turned them off.
pub fn enabled(dir: &Path) -> bool {
    offline_name::value(dir, KEY).as_deref() != Some("off")
}

/// Keep Skin Customization's LittleSkin Skins in `dir`'s horizon-player.txt.
pub fn set_enabled(dir: &Path, on: bool) -> std::io::Result<()> {
    offline_name::set_values(dir, &[(KEY, Some(if on { "on" } else { "off" }))])
}

/// Whether `name` can be a player's (1 to 16 letters, digits or `_`): an NPC's styled name is
/// never asked for.
pub fn is_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The skin of a LittleSkin profile answer, whose base64-encoded `textures` property carries
/// `{"textures": {"SKIN": {"url": ..., "metadata": {"model": "slim"}}}}`; None when it names
/// none. A cape is left out: one that failed to download would fail the skin with it
/// (`skins::lookup`).
pub fn parse(json: &str) -> Option<Textures> {
    let answer: serde_json::Value = serde_json::from_str(json).ok()?;
    let encoded = answer["properties"]
        .as_array()?
        .iter()
        .find(|property| property["name"] == "textures")?
        ["value"]
        .as_str()?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let skin = &payload["textures"]["SKIN"];
    let url = skin["url"].as_str()?;
    Some(Textures {
        skin: Some(url.to_owned()),
        slim: skin["metadata"]["model"].as_str() == Some("slim"),
        ..Textures::default()
    })
}

/// The most names kept on disk: a session on a busy server does not grow the file without end.
const KEPT_NAMES: usize = 512;

/// What LittleSkin answered for a name: its skin, or None for a name without one.
type Answer = Option<Textures>;

/// Names asked of LittleSkin and what it answered, this session and the last ones.
pub struct Lookup {
    /// This session's answers, by the name in lower case (LittleSkin tells names apart without case).
    answers: HashMap<String, Answer>,
    /// Asked this session, not answered yet.
    asked: HashSet<String>,
    /// The answers kept on disk (`keep_in`), shown until this session's come.
    kept: HashMap<String, Answer>,
    file: Option<PathBuf>,
    /// None when LittleSkin gave no answer (offline, a timeout): what was kept stays.
    send: Sender<(String, Option<Answer>)>,
    done: Receiver<(String, Option<Answer>)>,
}

impl Default for Lookup {
    fn default() -> Self {
        let (send, done) = channel();
        Self {
            answers: HashMap::new(),
            asked: HashSet::new(),
            kept: HashMap::new(),
            file: None,
            send,
            done,
        }
    }
}

impl Lookup {
    /// Keep the answers in `file`, and read the ones it holds.
    pub fn keep_in(&mut self, file: PathBuf) {
        self.kept = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| v.as_object().cloned())
            .map(|names| {
                names
                    .into_iter()
                    .map(|(name, answer)| (name, answer_of(&answer)))
                    .collect()
            })
            .unwrap_or_default();
        self.file = Some(file);
    }

    /// `name`'s skin on LittleSkin: this session's answer, else the one kept from the last (None
    /// before either, and for a name without one); asked the first time.
    pub fn textures(&mut self, name: &str) -> Option<Textures> {
        self.receive();
        if !is_name(name) {
            return None;
        }
        let key = name.to_ascii_lowercase();
        if let Some(answer) = self.answers.get(&key) {
            return answer.clone();
        }
        if self.asked.insert(key.clone()) {
            let send = self.send.clone();
            let ask = key.clone();
            let asked = std::thread::Builder::new()
                .name("littleskin".into())
                .spawn(move || {
                    let answer = fetch(&ask);
                    let _ = send.send((ask, answer));
                });
            if let Err(e) = asked {
                log::warn!("Horizon: LittleSkin skin: {e}");
            }
        }
        self.kept.get(&key).cloned().flatten()
    }

    /// Whether `name` waits for LittleSkin with nothing kept to show meanwhile.
    pub fn waiting(&mut self, name: &str) -> bool {
        self.receive();
        let key = name.to_ascii_lowercase();
        is_name(name)
            && !self.answers.contains_key(&key)
            && !self.kept.contains_key(&key)
            && self.asked.contains(&key)
    }

    /// The answers that came, kept on disk when they changed what was kept.
    fn receive(&mut self) {
        let mut changed = false;
        while let Ok((key, answer)) = self.done.try_recv() {
            // No answer: what was kept stands for this session.
            let answer = match answer {
                Some(answer) => answer,
                None => self.kept.get(&key).cloned().flatten(),
            };
            if self.kept.get(&key) != Some(&answer) {
                if self.kept.len() >= KEPT_NAMES
                    && !self.kept.contains_key(&key)
                    && let Some(old) = self.kept.keys().next().cloned()
                {
                    self.kept.remove(&old);
                }
                self.kept.insert(key.clone(), answer.clone());
                changed = true;
            }
            self.answers.insert(key, answer);
        }
        if changed && let Some(file) = &self.file {
            let names: serde_json::Map<String, serde_json::Value> = self
                .kept
                .iter()
                .map(|(name, answer)| (name.clone(), value_of(answer)))
                .collect();
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(file, serde_json::Value::Object(names).to_string()) {
                log::warn!("Horizon: could not keep LittleSkin's skins: {e}");
            }
        }
    }
}

/// An answer as the file keeps it: `{"skin": url, "slim": bool}`, or null.
fn value_of(answer: &Answer) -> serde_json::Value {
    match answer {
        Some(t) => serde_json::json!({"skin": t.skin, "slim": t.slim}),
        None => serde_json::Value::Null,
    }
}

fn answer_of(value: &serde_json::Value) -> Answer {
    Some(Textures {
        skin: Some(value["skin"].as_str()?.to_owned()),
        slim: value["slim"].as_bool() == Some(true),
        ..Textures::default()
    })
}

/// LittleSkin's answer for `name`: Some(its skin, or None for a name without one), None when it
/// gave no answer.
fn fetch(name: &str) -> Option<Answer> {
    use std::io::Read;
    // No profile for the name: 204, or 200 with an empty answer (a name without a skin).
    let response = match ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .post(PROFILES)
        .set("Content-Type", "application/json; charset=utf-8")
        .send_string(&serde_json::json!([{"name": name}]).to_string())
    {
        Ok(response) => response,
        Err(ureq::Error::Status(204, _)) => return Some(None),
        Err(_) => return None,
    };
    if response.status() != 200 {
        return None;
    }
    let mut body = String::new();
    response
        .into_reader()
        .take(64 << 10)
        .read_to_string(&mut body)
        .ok()?;
    let profiles: serde_json::Value = serde_json::from_str(&body).ok()?;
    let first = profiles.as_array()?.first()?.to_string();
    Some(parse(&first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skin_and_its_model_from_the_answer() {
        let wide = r#"{"id":"64","name":"Wide","properties":[{"name":"textures","value":"eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly9saXR0bGVza2luLmNuL3N0b3JhZ2Uvc2tpbnMvd2lkZS5wbmcifX19"}]}"#;
        let t = parse(wide).unwrap();
        assert_eq!(
            t.skin.as_deref(),
            Some("http://littleskin.cn/storage/skins/wide.png")
        );
        assert!(!t.slim && !t.signed && t.cape.is_none());
        let slim = r#"{"id":"64","name":"Slim","properties":[{"name":"textures","value":"eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly9saXR0bGVza2luLmNuL3N0b3JhZ2Uvc2tpbnMvYS5wbmciLCJtZXRhZGF0YSI6eyJtb2RlbCI6InNsaW0ifX19fQ=="}]}"#;
        let t = parse(slim).unwrap();
        assert!(t.slim);
        assert!(t.cape.is_none());
        assert!(parse("").is_none());
        // A profile without a textures property, and one whose skin is only a cape.
        assert!(parse(r#"{"id":"64","name":"Bare","properties":[]}"#).is_none());
        assert!(parse(r#"{"id":"64","name":"Caped","properties":[{"name":"textures","value":"eyJ0ZXh0dXJlcyI6eyJDQVBFIjp7InVybCI6Imh0dHA6Ly9saXR0bGVza2luLmNuL3N0b3JhZ2UvY2xvYWtzL2IucG5nIn19fQ=="}]}"#).is_none());
    }

    #[test]
    fn only_player_names_are_asked() {
        assert!(is_name("ErickSkrauch"));
        assert!(is_name("a_1"));
        assert!(!is_name(""));
        assert!(!is_name("§6Shopkeeper"));
        assert!(!is_name("seventeen_letters"));
        assert!(!is_name("two words"));
        let mut lookup = Lookup::default();
        assert!(lookup.textures("§cNPC").is_none());
        assert!(lookup.answers.is_empty());
    }

    #[test]
    fn turned_off_and_on_in_the_player_file() {
        let dir = std::env::temp_dir().join(format!("horizon-littleskin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(enabled(&dir));
        set_enabled(&dir, false).unwrap();
        assert!(!enabled(&dir));
        set_enabled(&dir, true).unwrap();
        assert!(enabled(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn answers_kept_on_disk_show_at_the_next_start() {
        let dir = std::env::temp_dir().join(format!("horizon-littleskin-kept-{}", std::process::id()));
        let file = dir.join("littleskin.json");
        let _ = std::fs::remove_dir_all(&dir);
        let skin = Textures {
            skin: Some("http://littleskin.cn/storage/skins/a.png".into()),
            slim: true,
            ..Textures::default()
        };
        let mut lookup = Lookup::default();
        lookup.keep_in(file.clone());
        // As the asking thread would answer, without asking.
        lookup
            .asked
            .extend(["steve".to_owned(), "nobody".to_owned()]);
        lookup
            .send
            .send(("steve".into(), Some(Some(skin.clone()))))
            .unwrap();
        lookup.send.send(("nobody".into(), Some(None))).unwrap();
        assert_eq!(lookup.textures("Steve"), Some(skin.clone()));
        assert_eq!(lookup.textures("nobody"), None);
        let mut next = Lookup::default();
        next.keep_in(file);
        next.asked
            .extend(["steve".to_owned(), "nobody".to_owned(), "new".to_owned()]);
        assert_eq!(next.textures("STEVE"), Some(skin));
        assert!(!next.waiting("steve") && !next.waiting("nobody"));
        assert!(next.waiting("new"));
        // No answer this time: what was kept stands.
        next.send.send(("steve".into(), None)).unwrap();
        assert!(next.textures("steve").is_some_and(|t| t.slim));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "asks littleskin.cn"]
    fn littleskin_answers_a_known_name() {
        let t = fetch("Notch").expect("an answer").expect("a skin");
        assert!(t.skin.unwrap().contains("littleskin"));
        assert_eq!(fetch("zzzznosuchname123"), Some(None));
    }
}
