use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

pub const SKILL: &str = r#"---
name: blastray
description: Use as the first structural investigation for an unfamiliar code task, before broad repository grep or source reading, when it can reduce exploration. Find and inspect the most plausible implementation location; do not use for exact literals, config/docs-only work, writing, or admin.
---

BlastRay is a structural first pass meant to reduce repository exploration.

For an unfamiliar code task, call `find` once with the user's actual task before
broad grep/read. Inspect the best plausible result first. If that supplies the
implementation context needed, stop exploring and continue the task. Refine the
find or inspect another candidate only when the evidence does not answer it.

The four tools are not a checklist: use `trace` only for a known A-to-B call
path, and `impact` only when change blast radius matters. Inspect source is
already read; reopen it only for omitted local detail. Use normal grep/read for
exact literals, config/docs, unsupported syntax/languages, or incomplete
evidence. Find relevance is suggestive; confirmed graph facts are conservative.
"#;

pub fn setup() -> Result<String, String> {
    let home = user_home()?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the installed BlastRay binary: {error}"))?;
    setup_with(&home, &executable, Path::new("codex"))
}

fn setup_with(home: &Path, executable: &Path, codex: &Path) -> Result<String, String> {
    let skill_path = home.join(".agents/skills/blastray/SKILL.md");
    check_skill_path(&skill_path)?;
    register_mcp(codex, executable)?;
    install_skill(&skill_path)?;

    Ok(format!(
        "Configured Codex for BlastRay.\nMCP server: blastray\nSkill: {}",
        skill_path.display()
    ))
}

fn user_home() -> Result<PathBuf, String> {
    user_home_with(cfg!(windows), std::env::var_os)
}

fn user_home_with(
    windows: bool,
    get: impl Fn(&'static str) -> Option<OsString>,
) -> Result<PathBuf, String> {
    let value = |name| get(name).filter(|value| !value.is_empty());
    if windows {
        if let Some(home) = value("USERPROFILE") {
            return Ok(PathBuf::from(home));
        }
        if let (Some(drive), Some(path)) = (value("HOMEDRIVE"), value("HOMEPATH")) {
            let mut home = drive;
            home.push(path);
            return Ok(PathBuf::from(home));
        }
    }
    value("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "could not determine the current user's home directory".to_string())
}

fn install_skill(path: &Path) -> Result<(), String> {
    if matches!(skill_state(path)?, SkillState::Installed) {
        return Ok(());
    }

    let parent = path
        .parent()
        .ok_or_else(|| "invalid Codex skill path".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    fs::write(path, SKILL).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn check_skill_path(path: &Path) -> Result<(), String> {
    skill_state(path).map(|_| ())
}

enum SkillState {
    Installed,
    Missing,
}

fn skill_state(path: &Path) -> Result<SkillState, String> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == SKILL => Ok(SkillState::Installed),
        Ok(_) => Err(format!(
            "refusing to replace an existing BlastRay Codex skill at {}",
            path.display()
        )),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("could not read {}: {error}", path.display()))
        }
        Err(_) => Ok(SkillState::Missing),
    }
}

fn register_mcp(codex: &Path, executable: &Path) -> Result<(), String> {
    let known = Command::new(codex)
        .args(["mcp", "get", "blastray", "--json"])
        .output()
        .map_err(|error| format!("could not run `codex mcp get blastray`: {error}"))?;
    if known.status.success() {
        let registration: CodexMcpRegistration =
            serde_json::from_slice(&known.stdout).map_err(|error| {
                format!(
                    "could not read existing Codex MCP registration for blastray as JSON: {error}"
                )
            })?;
        if registration.is_equivalent(executable) {
            return Ok(());
        }
        return Err(format!(
            "Codex MCP server 'blastray' already exists but is not this BlastRay executable with the `mcp` argument; refusing to overwrite it. Existing registration: {}",
            registration.summary()
        ));
    }

    let output = Command::new(codex)
        .args(["mcp", "add", "blastray", "--"])
        .arg(executable)
        .arg("mcp")
        .output()
        .map_err(|error| format!("could not run `codex mcp add`: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`codex mcp add blastray` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Deserialize)]
struct CodexMcpRegistration {
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    transport: CodexMcpTransport,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum CodexMcpTransport {
    #[serde(rename = "stdio")]
    Stdio { command: String, args: Vec<String> },
    #[serde(other)]
    Other,
}

impl CodexMcpRegistration {
    fn is_equivalent(&self, executable: &Path) -> bool {
        matches!(
            &self.transport,
            CodexMcpTransport::Stdio { command, args }
                if self.enabled && Path::new(command) == executable && args.as_slice() == ["mcp"]
        )
    }

    fn summary(&self) -> String {
        match &self.transport {
            CodexMcpTransport::Stdio { command, args } => {
                format!("stdio command={command:?} args={args:?}")
            }
            CodexMcpTransport::Other => "non-stdio transport".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SKILL, install_skill, setup_with, user_home_with};
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

    fn path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "blastray-codex-test-{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn installs_one_idempotent_skill_without_overwriting_a_custom_one() {
        let directory = path();
        let skill = directory.join(".agents/skills/blastray/SKILL.md");
        install_skill(&skill).unwrap();
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL);
        install_skill(&skill).unwrap();

        fs::write(&skill, "custom\n").unwrap();
        let error = install_skill(&skill).unwrap_err();
        assert!(error.contains("refusing to replace"));
        assert_eq!(fs::read_to_string(&skill).unwrap(), "custom\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn home_resolution_uses_windows_profile_before_unix_home() {
        let home = user_home_with(true, |name| match name {
            "USERPROFILE" => Some(OsString::from(r"C:\Users\BlastRay")),
            "HOME" => Some(OsString::from("/unexpected")),
            _ => None,
        })
        .unwrap();
        assert_eq!(home, Path::new(r"C:\Users\BlastRay"));

        let fallback = user_home_with(true, |name| match name {
            "HOMEDRIVE" => Some(OsString::from("C:")),
            "HOMEPATH" => Some(OsString::from(r"\Users\BlastRay")),
            _ => None,
        })
        .unwrap();
        assert_eq!(fallback, Path::new(r"C:\Users\BlastRay"));
    }

    #[cfg(unix)]
    #[test]
    fn setup_uses_codex_mcp_add_once_and_leaves_unrelated_config_alone() {
        use std::os::unix::fs::PermissionsExt;

        let directory = path();
        let home = directory.join("home");
        let config = home.join(".codex/config.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[unrelated]\nkeep = true\n").unwrap();
        let registration = directory.join("registered");
        let invocation = directory.join("invocation");
        let codex = directory.join("codex");
        fs::write(
            &codex,
            format!(
                "#!/bin/sh\nif [ \"$2\" = get ]; then\n  if [ -f {registration} ]; then\n    printf '%s\\n' '{{\"transport\":{{\"type\":\"stdio\",\"command\":\"/opt/blastray/bin/blastray\",\"args\":[\"mcp\"]}}}}'\n    exit 0\n  fi\n  exit 1\nfi\nprintf '%s\\n' \"$@\" > {invocation}\ntouch {registration}\n",
                registration = registration.display(),
                invocation = invocation.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();

        setup_with(&home, Path::new("/opt/blastray/bin/blastray"), &codex).unwrap();
        setup_with(&home, Path::new("/opt/blastray/bin/blastray"), &codex).unwrap();

        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "[unrelated]\nkeep = true\n"
        );
        assert_eq!(
            fs::read_to_string(&invocation).unwrap(),
            "mcp\nadd\nblastray\n--\n/opt/blastray/bin/blastray\nmcp\n"
        );
        assert_eq!(
            fs::read_to_string(home.join(".agents/skills/blastray/SKILL.md")).unwrap(),
            SKILL
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn setup_refuses_a_different_existing_mcp_registration() {
        use std::os::unix::fs::PermissionsExt;

        let directory = path();
        fs::create_dir_all(&directory).unwrap();
        let codex = directory.join("codex");
        fs::write(
            &codex,
            "#!/bin/sh\nif [ \"$2\" = get ]; then\n  printf '%s\\n' '{\"transport\":{\"type\":\"stdio\",\"command\":\"/custom/blastray\",\"args\":[\"mcp\"]}}'\n  exit 0\nfi\nexit 99\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();

        let error = setup_with(
            &directory.join("home"),
            Path::new("/opt/blastray/bin/blastray"),
            &codex,
        )
        .unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        assert!(error.contains("/custom/blastray"));
        assert!(
            !directory
                .join("home/.agents/skills/blastray/SKILL.md")
                .exists()
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
