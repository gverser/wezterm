//! The forms the sidebar shows: editing an ssh session, editing a local
//! session, and confirming something.
//!
//! These only build and read `FormState`s, so that the field rules can be
//! tested without a window; `termwindow::sidebar` puts them on screen.

use crate::sessions::ssh::{SshAuth, SshHost};
use crate::sessions::SessionSpawn;
use crate::termwindow::form::{Field, FormButton, FormState};
use config::keyassignment::SpawnTabDomain;
use std::path::PathBuf;

pub const SSH_FIELDS: [&str; 7] = [
    "名称",
    "主机",
    "端口",
    "用户名",
    "认证方式",
    "密码",
    "私钥文件",
];
const AUTH_PASSWORD: &str = "密码";
const AUTH_KEY: &str = "私钥";

pub fn ssh_form(name: &str, host: Option<&SshHost>) -> FormState {
    let title = if host.is_some() {
        "编辑 SSH 会话"
    } else {
        "新建 SSH 会话"
    };
    let blank = SshHost {
        host: String::new(),
        port: None,
        user: String::new(),
        auth: SshAuth::Password,
        password: String::new(),
        key_path: String::new(),
    };
    let h = host.unwrap_or(&blank);
    let auth = match h.auth {
        SshAuth::Password => AUTH_PASSWORD,
        SshAuth::Key => AUTH_KEY,
    };
    let mut state = FormState::new(
        title,
        vec![
            Field::text(SSH_FIELDS[0], name),
            Field::text(SSH_FIELDS[1], &h.host),
            Field::text(
                SSH_FIELDS[2],
                &h.port.map(|p| p.to_string()).unwrap_or_default(),
            )
            .with_placeholder("默认 22"),
            Field::text(SSH_FIELDS[3], &h.user),
            Field::choice(SSH_FIELDS[4], &[AUTH_PASSWORD, AUTH_KEY], auth),
            Field::password(SSH_FIELDS[5], &h.password),
            Field::text(SSH_FIELDS[6], &h.key_path),
        ],
        vec![
            FormButton::Primary("保存".to_string()),
            FormButton::Cancel("取消".to_string()),
            FormButton::Extra("浏览…".to_string(), 6),
        ],
    );
    apply_auth_choice(&mut state);
    // A new session starts at the host, the one field that must be filled.
    if host.is_none() {
        state.focus_field(1);
    }
    state
}

/// A blank port is fine and means "not set"; anything else must be a port.
fn parse_port(text: &str) -> Result<Option<u16>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(()),
        Ok(port) => Ok(Some(port)),
    }
}

pub fn validate_ssh_form(state: &FormState) -> Vec<(usize, String)> {
    let mut errors = vec![];
    if state.value("主机").trim().is_empty() {
        errors.push((state.index_of("主机").unwrap(), "主机不能为空".to_string()));
    }
    if parse_port(state.value("端口")).is_err() {
        errors.push((
            state.index_of("端口").unwrap(),
            "端口必须是 1–65535 的整数".to_string(),
        ));
    }
    if state.value("认证方式") == AUTH_KEY && state.value("私钥文件").trim().is_empty() {
        errors.push((
            state.index_of("私钥文件").unwrap(),
            "私钥文件不能为空".to_string(),
        ));
    }
    errors
}

/// The node name and host the form describes. Call after validating.
pub fn ssh_form_values(state: &FormState) -> (String, SshHost) {
    let host = state.value("主机").trim().to_string();
    let name = match state.value("名称").trim() {
        "" => host.clone(),
        name => name.to_string(),
    };
    (
        name,
        SshHost {
            host,
            port: parse_port(state.value("端口")).unwrap_or(None),
            user: state.value("用户名").trim().to_string(),
            auth: if state.value("认证方式") == AUTH_KEY {
                SshAuth::Key
            } else {
                SshAuth::Password
            },
            // Spaces can be part of a password.
            password: state.value("密码").to_string(),
            key_path: state.value("私钥文件").trim().to_string(),
        },
    )
}

/// Only the field the chosen method uses can be edited.
pub fn apply_auth_choice(state: &mut FormState) {
    let key = state.value("认证方式") == AUTH_KEY;
    if let Some(i) = state.index_of("密码") {
        state.fields[i].enabled = !key;
    }
    if let Some(i) = state.index_of("私钥文件") {
        state.fields[i].enabled = key;
    }
    if !state
        .fields
        .get(state.focus)
        .map(|f| f.enabled)
        .unwrap_or(true)
    {
        state.focus_next();
    }
}

fn domain_to_string(domain: &SpawnTabDomain) -> String {
    match domain {
        SpawnTabDomain::DefaultDomain | SpawnTabDomain::CurrentPaneDomain => String::new(),
        SpawnTabDomain::DomainName(name) => name.clone(),
        SpawnTabDomain::DomainId(id) => id.to_string(),
    }
}

/// The form for a local session; `new` when it is to be created on saving.
pub fn local_form(new: bool, name: &str, spawn: &SessionSpawn) -> FormState {
    let title = if new { "新建会话" } else { "编辑会话" };
    let command = spawn
        .args
        .as_ref()
        .and_then(|args| shlex::try_join(args.iter().map(|s| s.as_str())).ok())
        .unwrap_or_default();
    FormState::new(
        title,
        vec![
            Field::text("名称", name),
            Field::text("域", &domain_to_string(&spawn.domain)),
            Field::text(
                "目录",
                &spawn
                    .cwd
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            Field::text("命令", &command),
        ],
        vec![
            FormButton::Primary("保存".to_string()),
            FormButton::Cancel("取消".to_string()),
        ],
    )
}

pub fn validate_local_form(state: &FormState) -> Vec<(usize, String)> {
    let command = state.value("命令").trim();
    if !command.is_empty() && shlex::split(command).map_or(true, |a| a.is_empty()) {
        return vec![(state.index_of("命令").unwrap(), "命令无法解析".to_string())];
    }
    vec![]
}

/// The node name and spawn the form describes. Blank fields mean "the
/// default", as in the prompts this form replaced; `env` is not on the form
/// and is carried over from `old`.
pub fn local_form_values(state: &FormState, old: &SessionSpawn) -> (String, SessionSpawn) {
    let domain = match state.value("域").trim() {
        "" => SpawnTabDomain::DefaultDomain,
        name => SpawnTabDomain::DomainName(name.to_string()),
    };
    let cwd = match state.value("目录").trim() {
        "" => None,
        dir => Some(PathBuf::from(dir)),
    };
    let args = match state.value("命令").trim() {
        "" => None,
        command => shlex::split(command).filter(|a| !a.is_empty()),
    };
    (
        state.value("名称").trim().to_string(),
        SessionSpawn {
            domain,
            cwd,
            args,
            env: old.env.clone(),
        },
    )
}

/// Asked before a session that still has terminals running under it is
/// removed from the tree. Only the entry goes; the terminals keep running.
pub fn delete_session_form(name: &str) -> FormState {
    let mut state = danger_confirm_form("删除会话", "", "删除");
    state.message = vec![
        format!("从侧边栏删除「{name}」吗？"),
        "其下正在运行的终端会继续运行，只删除保存的条目。".to_string(),
    ];
    state
}

/// `confirm_form` for something that can't be undone: the button that does
/// it is red.
pub fn danger_confirm_form(title: &str, message: &str, ok: &str) -> FormState {
    let mut state = confirm_form(title, message, ok);
    state.danger = true;
    state
}

pub fn confirm_form(title: &str, message: &str, ok: &str) -> FormState {
    let mut state = FormState::new(
        title,
        vec![],
        vec![
            FormButton::Primary(ok.to_string()),
            FormButton::Cancel("取消".to_string()),
        ],
    );
    // A form draws each message line as a row and has no use for line
    // breaks inside one; blank lines go too, the card spaces itself.
    state.message = message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    state
}

#[cfg(test)]
mod test {
    use super::*;
    use config::keyassignment::SpawnTabDomain;
    use std::path::PathBuf;

    fn saved() -> SshHost {
        SshHost {
            host: "10.0.0.5".to_string(),
            port: Some(2222),
            user: "root".to_string(),
            auth: SshAuth::Password,
            password: "pw".to_string(),
            key_path: String::new(),
        }
    }

    fn set(state: &mut FormState, label: &str, value: &str) {
        let i = state.index_of(label).unwrap();
        state.fields[i].value = value.to_string();
    }

    #[test]
    fn new_ssh_form_leaves_port_blank() {
        let state = ssh_form("", None);
        let port = state.index_of("端口").unwrap();
        assert_eq!(state.value("端口"), "");
        assert_eq!(state.fields[port].placeholder, "默认 22");
        assert_eq!(state.value("认证方式"), "密码");
        assert_eq!(state.title, "新建 SSH 会话");
        let labels: Vec<&str> = state.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, SSH_FIELDS.to_vec());
    }

    #[test]
    fn existing_ssh_form_shows_saved_values() {
        let state = ssh_form("web01", Some(&saved()));
        assert_eq!(state.title, "编辑 SSH 会话");
        assert_eq!(state.value("名称"), "web01");
        assert_eq!(state.value("主机"), "10.0.0.5");
        assert_eq!(state.value("端口"), "2222");
        assert_eq!(state.value("用户名"), "root");
        assert_eq!(state.value("密码"), "pw");
    }

    #[test]
    fn empty_host_is_rejected() {
        let state = ssh_form("", None);
        let errors = validate_ssh_form(&state);
        assert_eq!(
            errors,
            vec![(state.index_of("主机").unwrap(), "主机不能为空".to_string())]
        );
    }

    #[test]
    fn port_out_of_range_is_rejected() {
        for bad in ["0", "65536", "abc"] {
            let mut state = ssh_form("", None);
            set(&mut state, "主机", "h");
            set(&mut state, "端口", bad);
            let errors = validate_ssh_form(&state);
            assert_eq!(
                errors,
                vec![(
                    state.index_of("端口").unwrap(),
                    "端口必须是 1–65535 的整数".to_string()
                )],
                "port {bad:?}"
            );
        }
    }

    #[test]
    fn a_typed_port_is_kept() {
        let mut state = ssh_form("", None);
        set(&mut state, "主机", "h");
        set(&mut state, "端口", " 2200 ");
        assert!(validate_ssh_form(&state).is_empty());
        assert_eq!(ssh_form_values(&state).1.port, Some(2200));
    }

    #[test]
    fn key_auth_requires_key_path() {
        let mut state = ssh_form("", None);
        set(&mut state, "主机", "h");
        set(&mut state, "认证方式", "私钥");
        apply_auth_choice(&mut state);
        let errors = validate_ssh_form(&state);
        assert_eq!(
            errors,
            vec![(
                state.index_of("私钥文件").unwrap(),
                "私钥文件不能为空".to_string()
            )]
        );
    }

    #[test]
    fn blank_name_falls_back_to_host() {
        let mut state = ssh_form("", None);
        set(&mut state, "主机", " 10.0.0.9 ");
        set(&mut state, "用户名", " tester ");
        set(&mut state, "密码", " p w ");
        let (name, host) = ssh_form_values(&state);
        assert_eq!(name, "10.0.0.9");
        assert_eq!(host.host, "10.0.0.9");
        assert_eq!(host.user, "tester");
        assert_eq!(host.port, None, "a blank port leaves it to ~/.ssh/config");
        assert_eq!(host.password, " p w ", "passwords are kept verbatim");
        assert_eq!(host.auth, SshAuth::Password);
    }

    #[test]
    fn key_choice_maps_to_key_auth() {
        let mut state = ssh_form("n", None);
        set(&mut state, "主机", "h");
        set(&mut state, "认证方式", "私钥");
        set(&mut state, "私钥文件", r" C:\k\id ");
        let (_, host) = ssh_form_values(&state);
        assert_eq!(host.auth, SshAuth::Key);
        assert_eq!(host.key_path, r"C:\k\id");
    }

    #[test]
    fn auth_choice_toggles_fields() {
        let mut state = ssh_form("", None);
        let pw = state.index_of("密码").unwrap();
        let key = state.index_of("私钥文件").unwrap();
        apply_auth_choice(&mut state);
        assert!(state.fields[pw].enabled);
        assert!(!state.fields[key].enabled);
        set(&mut state, "认证方式", "私钥");
        state.focus = pw;
        apply_auth_choice(&mut state);
        assert!(!state.fields[pw].enabled);
        assert!(state.fields[key].enabled);
        assert_ne!(state.focus, pw, "focus moves off a field that got disabled");
    }

    #[test]
    fn local_form_round_trips_spawn() {
        let old = SessionSpawn {
            domain: SpawnTabDomain::DomainName("main".to_string()),
            cwd: Some(PathBuf::from("D:/work")),
            args: Some(vec!["bash".to_string()]),
            env: vec![("A".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
        };
        let mut state = local_form(false, "casb", &old);
        assert_eq!(state.value("域"), "main");
        assert_eq!(state.value("目录"), "D:/work");
        assert_eq!(state.value("命令"), "bash");
        set(&mut state, "域", "");
        set(&mut state, "目录", " ");
        set(&mut state, "命令", "htop -d 5");
        assert!(validate_local_form(&state).is_empty());
        let (name, spawn) = local_form_values(&state, &old);
        assert_eq!(name, "casb");
        assert_eq!(spawn.domain, SpawnTabDomain::DefaultDomain);
        assert_eq!(spawn.cwd, None);
        assert_eq!(
            spawn.args,
            Some(vec!["htop".to_string(), "-d".to_string(), "5".to_string()])
        );
        assert_eq!(spawn.env, old.env, "env is not on the form and is kept");
    }

    #[test]
    fn a_local_form_is_titled_for_what_it_does() {
        let spawn = SessionSpawn::default();
        assert_eq!(local_form(true, "新建会话", &spawn).title, "新建会话");
        assert_eq!(local_form(false, "casb", &spawn).title, "编辑会话");
    }

    #[test]
    fn unparsable_command_is_rejected() {
        let mut state = local_form(false, "x", &SessionSpawn::default());
        set(&mut state, "命令", "echo \"unterminated");
        assert_eq!(
            validate_local_form(&state),
            vec![(state.index_of("命令").unwrap(), "命令无法解析".to_string())]
        );
    }

    #[test]
    fn deleting_a_session_with_terminals_asks_in_red_and_says_they_keep_running() {
        let state = delete_session_form("web-01");
        assert!(state.danger);
        assert_eq!(state.title, "删除会话");
        assert_eq!(
            state.message,
            vec![
                "从侧边栏删除「web-01」吗？".to_string(),
                "其下正在运行的终端会继续运行，只删除保存的条目。".to_string(),
            ]
        );
        assert_eq!(state.shortcut('y'), Some(true));
    }

    #[test]
    fn a_message_over_several_lines_is_shown_line_by_line() {
        let prompt = crate::sessions::close_prompt(&[("casb".to_string(), 1)]).unwrap();
        let state = confirm_form("关闭窗口", &prompt, "关闭");
        assert_eq!(
            state.message,
            vec![
                "后台还有 1 个终端在运行：".to_string(),
                "    casb（1）".to_string(),
                "全部关闭并退出吗？".to_string(),
            ]
        );
    }

    #[test]
    fn confirm_form_has_message_and_two_buttons() {
        let state = confirm_form("覆盖文件", "远端已有同名文件「a」，覆盖吗？", "覆盖");
        assert!(state.fields.is_empty());
        assert_eq!(
            state.message,
            vec!["远端已有同名文件「a」，覆盖吗？".to_string()]
        );
        assert_eq!(
            state.buttons,
            vec![
                FormButton::Primary("覆盖".to_string()),
                FormButton::Cancel("取消".to_string())
            ]
        );
    }
}
