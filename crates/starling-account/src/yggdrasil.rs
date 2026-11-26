// Copyright (c) 2026 Ludovic Marie and the Horizon contributors.
// SPDX-License-Identifier: MIT
//! Any third-party skin server that speaks the Yggdrasil protocol. The wire format is the
//! authlib-injector standard `littleskin` already implements, so this reuses those helpers and
//! only adds what a generic server needs: a base URL the player gives us, and a texture host
//! that comes from that same server rather than being built in.
use super::*;

/// Signs in with a username and password (plus an optional `password:otp` second factor) to
/// the Yggdrasil server at `base`, and reads that server's textures for the profile.
pub fn authenticate(
    base: &str,
    name: &str,
    username: &str,
    password: &str,
    otp: &str,
) -> Result<(Account, Secrets), String> {
    let (mut account, secrets) = littleskin::authenticate_with(base, username, password, otp)?;
    account.provider = Provider::Yggdrasil;
    account.auth_server = base.to_string();
    account.auth_name = name.to_string();
    let mut secrets = secrets;
    secrets.provider = Provider::Yggdrasil;
    secrets.auth_server = base.to_string();
    let service = service_name(base);
    littleskin::update_textures_at(base, &mut account, &service);
    Ok((account, secrets))
}

/// Renews a skin server's session through its own `/authserver/refresh`.
pub fn refresh(account: &Account, secrets: &Secrets) -> Result<(Account, Secrets), String> {
    let base = server_of(account, secrets);
    let (mut account, secrets) = littleskin::refresh_with(&base, account, secrets)?;
    account.provider = Provider::Yggdrasil;
    account.auth_server = base.clone();
    if account.auth_name.is_empty() {
        account.auth_name = service_name(&base);
    }
    let mut secrets = secrets;
    secrets.provider = Provider::Yggdrasil;
    secrets.auth_server = base;
    let texture_base = account.auth_server.clone();
    let service = service_name(&texture_base);
    littleskin::update_textures_at(&texture_base, &mut account, &service);
    Ok((account, secrets))
}

/// A skin server's token with at least twenty minutes left, renewed otherwise.
pub fn fresh(account: &Account, secrets: Secrets) -> Result<(Account, Secrets), String> {
    if secrets.minecraft_expires_at > now() + 1200 {
        return Ok((account.clone(), secrets));
    }
    refresh(account, &secrets)
}

/// Checks a base URL is a live Yggdrasil server by asking it to authenticate a throwaway
/// client. A real server answers 400/401 with a structured `errorMessage`; something else at
/// that address (a website, a dead port) fails the request outright. Returns its server name
/// when it is one, so the UI can label the button after it.
pub fn verify(base: &str) -> Result<String, String> {
    let value = json!({
        "agent": {"name": "Minecraft", "version": 1},
        "username": "", "password": "", "clientToken": "starling-verify",
    });
    let service = service_name(base);
    match littleskin::exchange(&format!("{base}/authserver/authenticate"), &service, value) {
        Err(message) if message == littleskin::REFUSED => Ok(service),
        Err(_) => Err("starling.account.error.server_unreachable".into()),
        Ok(_) => Ok(service),
    }
}

fn server_of(account: &Account, secrets: &Secrets) -> String {
    if !account.auth_server.is_empty() {
        account.auth_server.clone()
    } else {
        secrets.auth_server().to_string()
    }
}

/// A label for error text: the host, or a generic name when the URL does not parse.
fn service_name(base: &str) -> String {
    base.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(base)
        .to_string()
}
