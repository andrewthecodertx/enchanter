//! Home directory resolution and first-run auto-init.
//!
//! The home-directory override pattern (ENV_HOME env var with fallback to
//! ~/.app) is adapted from hermes-agent's get_hermes_home()
//! (hermes-agent/hermes_constants.py).
//!
//! The scaffold-on-first-run pattern (auto-creating dirs + seed files when
//! ~/.enchanter doesn't exist) mirrors hermes-agent's bootstrap: when
//! ~/.hermes is absent, hermes seeding creates config.yaml, memory files,
//! and skill directories. See hermes-agent/setup-hermes.sh and
//! hermes-agent/hermes_constants.py for the directory layout that informed
//! our HOME_SUBDIRS and SCAFFOLD_FILES.

use std::path::PathBuf;

const HOME_SUBDIRS: &[&str] = &["memories", "skills"];

const SCAFFOLD_FILES: &[(&str, &str)] = &[
    ("SOUL.md", include_str!("scaffold/SOUL.md")),
    ("config.yaml", include_str!("scaffold/config.yaml")),
];

/// Resolve `~/.enchanter` or `ENCHANTER_HOME` override.
// Pattern adapted from hermes-agent's get_hermes_home()
// (hermes-agent/hermes_constants.py:43): env var override → fallback to
// ~/APP_DIR. hermes-agent also has profile-aware path resolution and
// context-var overrides; enchanter simplifies to env var + home dir.
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
// Pattern adapted from hermes-agent's bootstrap: setup-hermes.sh creates
// ~/.hermes/ with config.yaml, memory dirs, and skill dirs on first run.
// The seed-file approach (SOUL.md, config.yaml) mirrors hermes-agent's
// default SOUL.md seeding (hermes-agent/RELEASE_v0.3.0.md: "Seed a default
// global SOUL.md").
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