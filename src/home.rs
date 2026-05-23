//! Home directory resolution and first-run auto-init.

use std::path::PathBuf;

const HOME_SUBDIRS: &[&str] = &["memories", "skills"];

const SCAFFOLD_FILES: &[(&str, &str)] = &[
    ("SOUL.md", include_str!("scaffold/SOUL.md")),
    ("config.yaml", include_str!("scaffold/config.yaml")),
];

/// Resolve `~/.enchanter` or `ENCHANTER_HOME` override.
pub fn enchanter_home() -> PathBuf {
    std::env::var("ENCHANTER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("cannot find home directory")
                .join(".enchanter")
        })
}

/// Create the home directory scaffold on first run. Returns true if created.
pub fn init_home() -> anyhow::Result<bool> {
    let home = enchanter_home();
    if home.exists() {
        return Ok(false);
    }

    std::fs::create_dir_all(&home)?;

    for subdir in HOME_SUBDIRS {
        std::fs::create_dir_all(home.join(subdir))?;
    }

    for (path, content) in SCAFFOLD_FILES {
        std::fs::write(home.join(path), content)?;
    }

    Ok(true)
}