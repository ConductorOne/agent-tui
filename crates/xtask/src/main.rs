//! `cargo xtask` — repo automation that doesn't belong in any product crate.
//!
//! Run via:
//!
//! ```bash
//! cargo run -p xtask -- docs-coverage
//! ```
//!
//! (or, with `.cargo/config.toml` aliasing: `cargo xtask docs-coverage`.)

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check that every `##` heading in `skill-data/` carries a
    /// `<!-- tested-by: module::test_fn -->` annotation, and that
    /// every claimed test exists in the integration crate.
    DocsCoverage(DocsCoverageArgs),
    /// Check that every CLI subcommand + flag is mentioned in at
    /// least one skill page (or on `.undocumented-allowlist.txt`).
    /// Walks `agent-tui help` recursively to enumerate the surface.
    CliCoverage(CliCoverageArgs),
}

#[derive(clap::Args)]
struct CliCoverageArgs {
    /// Repo root.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Path to the built `agent-tui` binary. Defaults to
    /// `target/debug/agent-tui` under the repo root.
    #[arg(long)]
    bin: Option<PathBuf>,
}

#[derive(clap::Args)]
struct DocsCoverageArgs {
    /// Repo root. Defaults to the parent of `CARGO_MANIFEST_DIR`.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Skip the `cargo test --list` step — only check annotation
    /// presence. Useful for quick local feedback before a full build.
    #[arg(long)]
    no_test_list: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::DocsCoverage(args) => docs_coverage(&args),
        Cmd::CliCoverage(args) => cli_coverage(&args),
    }
}

fn docs_coverage(args: &DocsCoverageArgs) -> Result<()> {
    let repo = args
        .repo
        .clone()
        .or_else(|| {
            std::env::var_os("CARGO_MANIFEST_DIR")
                .map(PathBuf::from)
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let skill_data = repo.join("crates/agent-tui/skill-data");
    if !skill_data.is_dir() {
        bail!("skill-data not found at {}", skill_data.display());
    }

    let claims = collect_tested_by_claims(&skill_data)?;
    let total_md = claims.docs_visited;
    let total_headings = claims.headings.len();
    let total_claims = claims.tests_claimed.len();

    let mut errors: Vec<String> = Vec::new();

    for h in &claims.unannotated {
        errors.push(format!(
            "✗ {}:{} heading {:?} has no `<!-- tested-by: module::test_fn -->` annotation",
            h.file.display(),
            h.line,
            h.title,
        ));
    }

    let mut orphan_tests: BTreeSet<String> = BTreeSet::new();
    if !args.no_test_list {
        let known = list_integration_tests(&repo)?;
        // Build a suffix-set of the claimed test names for orphan
        // matching: a claim `vim_bwrap::foo` covers the bare `foo` too.
        let claimed_suffixes: BTreeSet<&str> = claims
            .tests_claimed
            .keys()
            .map(|k| k.rsplit("::").next().unwrap_or(k.as_str()))
            .collect();
        for (claim, where_) in &claims.tests_claimed {
            if !test_claim_matches(&known, claim) {
                errors.push(format!(
                    "✗ {}:{} declares tested-by `{}` but no such test exists",
                    where_.file.display(),
                    where_.line,
                    claim,
                ));
            }
        }
        // Orphan tests: integration tests that no doc claims. Soft warning.
        for t in &known {
            if !claimed_suffixes.contains(t.as_str()) {
                orphan_tests.insert(t.clone());
            }
        }
    }

    if !errors.is_empty() {
        eprintln!("docs-coverage: FAIL ({} issue(s))", errors.len());
        for e in &errors {
            eprintln!("  {e}");
        }
        if !orphan_tests.is_empty() {
            eprintln!(
                "  ⚠ {} test(s) not referenced from any doc (orphans):",
                orphan_tests.len()
            );
            for t in orphan_tests.iter().take(10) {
                eprintln!("    {t}");
            }
            if orphan_tests.len() > 10 {
                eprintln!("    … and {} more", orphan_tests.len() - 10);
            }
        }
        std::process::exit(1);
    }

    println!(
        "docs-coverage: PASS  ({} markdown file(s), {} heading(s), {} claim(s){})",
        total_md,
        total_headings,
        total_claims,
        if orphan_tests.is_empty() {
            String::new()
        } else {
            format!(", {} orphan test(s)", orphan_tests.len())
        }
    );
    Ok(())
}

#[derive(Debug)]
struct Heading {
    file: PathBuf,
    line: usize,
    title: String,
}

#[derive(Default)]
struct Claims {
    docs_visited: usize,
    headings: Vec<Heading>,
    unannotated: Vec<Heading>,
    /// `module::fn` → first location that claimed it (for diagnostics).
    tests_claimed: BTreeMap<String, Heading>,
}

fn collect_tested_by_claims(skill_data: &Path) -> Result<Claims> {
    let mut out = Claims::default();
    for entry in WalkDir::new(skill_data) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        out.docs_visited += 1;
        let content =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let mut in_code_fence = false;
        for (idx, raw) in lines.iter().enumerate() {
            let line = raw.trim_start();
            if line.starts_with("```") {
                in_code_fence = !in_code_fence;
                continue;
            }
            if in_code_fence {
                continue;
            }
            // We only police H2 headings — H1 is "page title" and H3+
            // are sub-sections inside a use-case.
            if let Some(rest) = line.strip_prefix("## ") {
                let title = rest.trim().to_string();
                let heading = Heading {
                    file: path.to_path_buf(),
                    line: idx + 1,
                    title,
                };
                // Look ahead for the `<!-- tested-by: … -->` comment.
                // Allow blank lines between heading and comment.
                let mut claim: Option<String> = None;
                for next in lines.iter().skip(idx + 1).take(4) {
                    let next = next.trim();
                    if next.is_empty() {
                        continue;
                    }
                    if let Some(rest) = next.strip_prefix("<!-- tested-by:") {
                        let rest = rest.trim_end_matches("-->").trim();
                        if !rest.is_empty() {
                            claim = Some(rest.to_string());
                        }
                    }
                    break;
                }
                match claim {
                    Some(c) => {
                        // Two sentinels, both explicit:
                        //  `navigation` — link/index section, no test possible.
                        //  `untested (<reason>)` — feature lacks coverage;
                        //    the reason becomes a docs+tests TODO that
                        //    surfaces in greps.
                        let is_sentinel = c == "navigation" || c.starts_with("untested");
                        if !is_sentinel {
                            out.tests_claimed.entry(c).or_insert_with(|| Heading {
                                file: heading.file.clone(),
                                line: heading.line,
                                title: heading.title.clone(),
                            });
                        }
                        out.headings.push(heading);
                    }
                    None => {
                        out.unannotated.push(heading);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Run `cargo test --list` on the integration crate and collect the
/// `module::test_fn` names it would run.
fn list_integration_tests(repo: &Path) -> Result<BTreeSet<String>> {
    // `--list` honors `--features`; we need bwrap-gated tests to be
    // listed. `--no-run` would build; `--list` requires `--no-fail-fast`
    // semantics. Use a structured output format if available.
    // Use `--all-features` so docker-gated AND bwrap-gated tests both
    // show up. We tolerate "feature combination didn't build" errors
    // — if a feature genuinely doesn't compile elsewhere, that's a
    // different problem and CI catches it.
    let mut cmd = Command::new("cargo");
    cmd.args([
        "test",
        "-p",
        "agent-tui-integration",
        "--all-features",
        "--no-fail-fast",
        "--",
        "--list",
        "--format=terse",
    ])
    .current_dir(repo)
    .stdin(Stdio::null());
    let out = cmd
        .output()
        .with_context(|| "spawn cargo test --list (is `cargo` on PATH?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "cargo test --list exited {}: {}",
            out.status,
            stderr.lines().take(20).collect::<Vec<_>>().join("\n")
        );
    }
    // `cargo test --list --format=terse` emits one `<test_fn>: test`
    // line per test on stdout. Binary→test association lives on
    // STDERR (`Running tests/foo.rs (…)`), but cross-stream
    // interleaving order isn't preserved by `Command::output()`.
    //
    // Tradeoff: rather than try to reconstruct binary names, we
    // collect just the test fn names. Docs can annotate as either
    // `module::test_fn` or bare `test_fn`; the comparison is by
    // *suffix*. That means a stale `wrongmod::test_fn` annotation
    // still resolves IF `test_fn` is unique across binaries. In
    // practice this codebase keeps test fn names globally unique, so
    // the suffix-match is decisive. The "stale module name" risk is
    // small because module/binary names rarely change.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut tests = BTreeSet::new();
    for line in stdout.lines() {
        if let Some(name) = line.strip_suffix(": test") {
            let name = name.trim();
            if !name.is_empty() {
                tests.insert(name.to_string());
            }
        }
    }
    Ok(tests)
}

/// Does the claimed `module::test_fn` (or bare `test_fn`) match any
/// known test? Match is by suffix on the last `::`-segment so docs
/// can write either form.
fn test_claim_matches(known: &BTreeSet<String>, claim: &str) -> bool {
    let suffix = claim.rsplit("::").next().unwrap_or(claim);
    known.contains(suffix) || known.contains(claim)
}

// ---- cli-coverage --------------------------------------------------------

fn cli_coverage(args: &CliCoverageArgs) -> Result<()> {
    let repo = args
        .repo
        .clone()
        .or_else(|| {
            std::env::var_os("CARGO_MANIFEST_DIR")
                .map(PathBuf::from)
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let bin = args
        .bin
        .clone()
        .unwrap_or_else(|| repo.join("target/debug/agent-tui"));
    if !bin.is_file() {
        bail!(
            "agent-tui binary not found at {} (run `cargo build -p agent-tui` first)",
            bin.display()
        );
    }
    let skill_data = repo.join("crates/agent-tui/skill-data");
    if !skill_data.is_dir() {
        bail!("skill-data not found at {}", skill_data.display());
    }

    let surface = collect_cli_surface(&bin)?;
    let docs_text = load_all_skill_text(&skill_data)?;
    let allowlist = load_allowlist(&skill_data.join(".undocumented-allowlist.txt"))?;

    let mut missing: Vec<String> = Vec::new();
    for entry in &surface {
        if entry.flag.is_none() {
            // Subcommand-level check: does ANY skill page mention it?
            let token = format!("agent-tui {}", entry.subcommand);
            if !docs_text.contains(&token) && !allowlist.contains(&entry.subcommand) {
                missing.push(format!("subcommand `{}`", entry.subcommand));
            }
        } else if let Some(flag) = &entry.flag {
            // Flag-level check: mentioned in the skill text? Allowlist
            // entries can be `"<subcmd> --<flag>"` or just `"--<flag>"`
            // (global flag form).
            let qual = format!("{} {flag}", entry.subcommand);
            let needle = format!("--{}", flag.trim_start_matches("--"));
            if docs_text.contains(&needle) {
                continue;
            }
            if allowlist.contains(&qual) || allowlist.contains(flag) {
                continue;
            }
            missing.push(format!(
                "flag `{}` on subcommand `{}`",
                flag, entry.subcommand
            ));
        }
    }

    if !missing.is_empty() {
        eprintln!(
            "cli-coverage: FAIL ({} undocumented item(s))",
            missing.len()
        );
        for m in &missing {
            eprintln!("  ✗ {m}");
        }
        eprintln!();
        eprintln!(
            "  Add a mention in `crates/agent-tui/skill-data/**/*.md` \
             or list the item in `crates/agent-tui/skill-data/.undocumented-allowlist.txt`."
        );
        std::process::exit(1);
    }
    println!(
        "cli-coverage: PASS  ({} subcommand-flag pair(s) covered)",
        surface.len()
    );
    Ok(())
}

#[derive(Debug)]
struct CliEntry {
    subcommand: String,
    /// `None` for the subcommand itself, `Some("--foo")` for a flag.
    flag: Option<String>,
}

fn collect_cli_surface(bin: &Path) -> Result<Vec<CliEntry>> {
    // Preferred path: ask the binary itself for the typed surface.
    // The hidden `__surface` subcommand returns a JSON tree.
    if let Ok(entries) = collect_via_dump(bin) {
        return Ok(entries);
    }
    // Fallback: parse `--help` text. Less accurate but works for
    // older binaries.
    collect_via_help(bin)
}

fn collect_via_dump(bin: &Path) -> Result<Vec<CliEntry>> {
    let out = Command::new(bin)
        .arg("__surface")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {} __surface", bin.display()))?;
    if !out.status.success() {
        bail!("__surface exited {}", out.status);
    }
    let tree: serde_json::Value =
        serde_json::from_slice(&out.stdout).with_context(|| "parse __surface JSON")?;
    let mut entries: Vec<CliEntry> = Vec::new();
    // Top-level global flags (no subcommand).
    if let Some(flags) = tree.get("flags").and_then(serde_json::Value::as_array) {
        for f in flags {
            if let Some(s) = f.as_str() {
                entries.push(CliEntry {
                    subcommand: "(global)".into(),
                    flag: Some(s.to_string()),
                });
            }
        }
    }
    if let Some(subs) = tree
        .get("subcommands")
        .and_then(serde_json::Value::as_array)
    {
        for s in subs {
            walk_surface_node("", s, &mut entries);
        }
    }
    Ok(entries)
}

fn walk_surface_node(parent: &str, sub: &serde_json::Value, out: &mut Vec<CliEntry>) {
    let name = sub
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if name == "help" || name.starts_with("__") {
        return;
    }
    let full = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent} {name}")
    };
    out.push(CliEntry {
        subcommand: full.clone(),
        flag: None,
    });
    if let Some(flags) = sub.get("flags").and_then(serde_json::Value::as_array) {
        for f in flags {
            if let Some(s) = f.as_str()
                && !is_global_flag(s)
            {
                out.push(CliEntry {
                    subcommand: full.clone(),
                    flag: Some(s.to_string()),
                });
            }
        }
    }
    if let Some(subs) = sub.get("subcommands").and_then(serde_json::Value::as_array) {
        for ss in subs {
            walk_surface_node(&full, ss, out);
        }
    }
}

fn collect_via_help(bin: &Path) -> Result<Vec<CliEntry>> {
    let mut out: Vec<CliEntry> = Vec::new();
    // Enumerate top-level subcommands from `agent-tui help`.
    let top = run_help(bin, &[])?;
    let mut subs: Vec<String> = Vec::new();
    let mut in_commands = false;
    for line in top.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.is_empty() || !line.starts_with("  ") {
                in_commands = false;
                continue;
            }
            // `  spawn      Spawn a PTY-backed pane …`
            if let Some(name) = line.split_whitespace().next()
                && name != "help"
            {
                subs.push(name.to_string());
            }
        }
    }
    // Also collect global flags from the top help.
    for f in flags_in_help(&top) {
        out.push(CliEntry {
            subcommand: "(global)".into(),
            flag: Some(f),
        });
    }
    // Per-subcommand: record the subcommand itself + each flag.
    for sub in subs {
        out.push(CliEntry {
            subcommand: sub.clone(),
            flag: None,
        });
        let help = run_help(bin, &[&sub])?;
        for f in flags_in_help(&help) {
            // Skip the global flags we already collected.
            if is_global_flag(&f) {
                continue;
            }
            out.push(CliEntry {
                subcommand: sub.clone(),
                flag: Some(f),
            });
        }
        // Recurse one level deeper (e.g. `daemon run`, `daemon shutdown`).
        if let Some(nested) = nested_subcommands_in_help(&help) {
            for n in nested {
                let qual = format!("{sub} {n}");
                out.push(CliEntry {
                    subcommand: qual.clone(),
                    flag: None,
                });
                let nhelp = run_help(bin, &[&sub, &n])?;
                for f in flags_in_help(&nhelp) {
                    if is_global_flag(&f) {
                        continue;
                    }
                    out.push(CliEntry {
                        subcommand: qual.clone(),
                        flag: Some(f),
                    });
                }
            }
        }
    }
    Ok(out)
}

fn run_help(bin: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("help").args(args).stdin(Stdio::null());
    let out = cmd
        .output()
        .with_context(|| format!("spawn {} help {:?}", bin.display(), args))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Pull `--flag` tokens from a `--help` body. We strip the value
/// parameter (`<NAME>`) and return just the flag spelling.
fn flags_in_help(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in help.lines() {
        let line = raw.trim_start();
        // clap renders options as `  -x, --long [<VAL>]` or `      --long`.
        if let Some(idx) = line.find("--") {
            // Make sure it's a flag declaration, not body prose.
            // Heuristic: the `--` is in the options table column,
            // which means the leading whitespace was 4+ spaces OR
            // the line starts with `-`.
            let leading = raw.len() - line.len();
            let is_option_line = raw.starts_with("  -") || leading >= 4;
            if !is_option_line {
                continue;
            }
            let after = &line[idx..];
            let token: String = after
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(*c, '=' | '<' | '`' | ']' | ')'))
                .collect();
            if token.len() > 2 {
                // Strip trailing punctuation.
                let token = token.trim_end_matches([',', '.']).to_string();
                out.insert(token);
            }
        }
    }
    out
}

fn is_global_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--session"
            | "--socket-dir"
            | "--engine"
            | "--json"
            | "--timeout"
            | "--content-boundaries"
            | "--max-output"
            | "--allowed-binaries"
            | "--help"
            | "--version"
    )
}

fn nested_subcommands_in_help(help: &str) -> Option<Vec<String>> {
    let mut subs: Vec<String> = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.is_empty() || !line.starts_with("  ") {
                break;
            }
            if let Some(name) = line.split_whitespace().next()
                && name != "help"
            {
                subs.push(name.to_string());
            }
        }
    }
    if subs.is_empty() { None } else { Some(subs) }
}

fn load_all_skill_text(skill_data: &Path) -> Result<String> {
    let mut all = String::new();
    for entry in WalkDir::new(skill_data) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        all.push_str(&std::fs::read_to_string(path)?);
        all.push('\n');
    }
    Ok(all)
}

fn load_allowlist(path: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    if !path.is_file() {
        return Ok(out);
    }
    for line in std::fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.insert(line.to_string());
    }
    Ok(out)
}
