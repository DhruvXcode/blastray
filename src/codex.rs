use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SKILL: &str = r#"---
name: blastray
description: Use for unfamiliar code investigation, debugging, understanding implementation before editing, tracing behavior between known symbols, or assessing a shared change's blast radius in a locally indexed repository. Do not use for writing, admin, or config/docs-only work.
---

Use BlastRay when its MCP server is available and structural code context would help.

- For a vague task or unknown location, call `find` with the user's task in natural language.
- For a likely or known symbol, call `inspect`; its source context is already read unless you need omitted local detail.
- For a known A-to-B behavior path, call `trace`.
- Before a shared or structural change, call `impact(symbol)`; after edits, call `impact("@diff")`.

Use normal grep/read for exact literals, config or docs, unsupported syntax/languages,
empty or incomplete evidence, or omitted source detail. Find relevance is suggestive;
confirmed graph relationships are conservative. Empty or unresolved results do not prove
an implementation or runtime path is impossible. The user's task remains higher priority.
"#;

pub fn setup() -> Result<String, String> {
    let home = user_home()?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the installed BlastRay binary: {error}"))?;
    setup_with(&home, &executable, Path::new("codex"))
}

fn setup_with(home: &Path, executable: &Path, codex: &Path) -> Result<String, String> {
    let skill_path = home.join(".agents/skills/blastray/SKILL.md");
    install_skill(&skill_path)?;
    register_mcp(codex, executable)?;

    Ok(format!(
        "Configured Codex for BlastRay.\nMCP server: blastray\nSkill: {}",
        skill_path.display()
    ))
}

fn user_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "could not determine the current user's home directory".to_string())
}

fn install_skill(path: &Path) -> Result<(), String> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == SKILL => return Ok(()),
        Ok(_) => {
            return Err(format!(
                "refusing to replace an existing BlastRay Codex skill at {}",
                path.display()
            ));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("could not read {}: {error}", path.display()));
        }
        Err(_) => {}
    }

    let parent = path
        .parent()
        .ok_or_else(|| "invalid Codex skill path".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    fs::write(path, SKILL).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn register_mcp(codex: &Path, executable: &Path) -> Result<(), String> {
    let known = Command::new(codex)
        .args(["mcp", "get", "blastray"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("could not run `codex mcp get blastray`: {error}"))?;
    if known.success() {
        return Ok(());
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

#[cfg(test)]
mod tests {
    use super::{SKILL, install_skill, setup_with};
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
                "#!/bin/sh\nif [ \"$2\" = get ]; then [ -f {registration} ] && exit 0; exit 1; fi\nprintf '%s\\n' \"$@\" > {invocation}\ntouch {registration}\n",
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
}
