//! Embedded skill content served by `agent-tui skills get / list`.
//!
//! Modeled on Vercel Labs' `agent-browser skills` system. The canonical
//! content lives under `crates/agent-tui/skill-data/**` and is bundled
//! into the binary via `include_str!` so skill text is always version-
//! locked to the binary that serves it. See `docs/skills-rfc.md`.

/// One skill package. `body` is the canonical SKILL.md; `references`
/// and `templates` are sidecar files exposed when `--full` is set.
pub struct Skill {
    pub name: &'static str,
    pub description: &'static str,
    pub body: &'static str,
    pub references: &'static [(&'static str, &'static str)],
    pub templates: &'static [(&'static str, &'static str)],
}

const CORE: Skill = Skill {
    name: "core",
    description: include_str!("../skill-data/core/_description.txt"),
    body: include_str!("../skill-data/core/SKILL.md"),
    references: &[
        (
            "commands.md",
            include_str!("../skill-data/core/references/commands.md"),
        ),
        (
            "snapshot-refs.md",
            include_str!("../skill-data/core/references/snapshot-refs.md"),
        ),
        (
            "wait-and-events.md",
            include_str!("../skill-data/core/references/wait-and-events.md"),
        ),
    ],
    templates: &[],
};

const SHELL: Skill = Skill {
    name: "shell",
    description: include_str!("../skill-data/shell/_description.txt"),
    body: include_str!("../skill-data/shell/SKILL.md"),
    references: &[],
    templates: &[],
};

const VIM: Skill = Skill {
    name: "vim",
    description: include_str!("../skill-data/vim/_description.txt"),
    body: include_str!("../skill-data/vim/SKILL.md"),
    references: &[],
    templates: &[],
};

const AI_CLI: Skill = Skill {
    name: "ai-cli",
    description: include_str!("../skill-data/ai-cli/_description.txt"),
    body: include_str!("../skill-data/ai-cli/SKILL.md"),
    references: &[],
    templates: &[],
};

const TUI_APPS: Skill = Skill {
    name: "tui-apps",
    description: include_str!("../skill-data/tui-apps/_description.txt"),
    body: include_str!("../skill-data/tui-apps/SKILL.md"),
    references: &[],
    templates: &[],
};

const INTENT: Skill = Skill {
    name: "intent",
    description: include_str!("../skill-data/intent/_description.txt"),
    body: include_str!("../skill-data/intent/SKILL.md"),
    references: &[],
    templates: &[],
};

const ADDRESSING: Skill = Skill {
    name: "addressing",
    description: include_str!("../skill-data/addressing/_description.txt"),
    body: include_str!("../skill-data/addressing/SKILL.md"),
    references: &[],
    templates: &[],
};

/// Every skill bundled into this binary, in display order.
pub const ALL_SKILLS: &[&Skill] = &[
    &CORE,
    &INTENT,
    &ADDRESSING,
    &SHELL,
    &VIM,
    &AI_CLI,
    &TUI_APPS,
];

/// Look up a skill by name. Case-sensitive; matches `Skill::name` exactly.
#[must_use]
pub fn find(name: &str) -> Option<&'static Skill> {
    ALL_SKILLS.iter().copied().find(|s| s.name == name)
}

/// Render a skill for stdout.
///
/// When `full` is true, the body is followed by each reference and each
/// template, separated by a nonced fence so an agent reader can split
/// the stream back into parts cleanly. Form-feed (`\f`) would also work
/// — chose nonced markdown rules instead so the output stays printable.
#[must_use]
pub fn render(skill: &Skill, full: bool) -> String {
    let mut out = skill.body.to_string();
    if !full {
        return out;
    }
    for (path, content) in skill.references {
        out.push_str("\n\n");
        out.push_str(&fence(&format!("references/{path}")));
        out.push_str(content);
    }
    for (path, content) in skill.templates {
        out.push_str("\n\n");
        out.push_str(&fence(&format!("templates/{path}")));
        out.push_str(content);
    }
    out
}

fn fence(label: &str) -> String {
    format!("---\n<!-- skill-section: {label} -->\n---\n\n")
}
