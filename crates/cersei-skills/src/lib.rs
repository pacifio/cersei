//! cersei-skills — agentskills.io-compatible skill registry for the Cersei SDK.
//!
//! A **skill** is a markdown file with YAML frontmatter that documents
//! reusable procedural knowledge an agent can consult before acting. The
//! format matches the [agentskills.io](https://agentskills.io/) open standard
//! byte-for-byte so skills are portable across runtimes.
//!
//! This crate is deliberately small: parse, load, lookup. No LLM calls, no
//! network, no filesystem writes outside `crates/cersei-tools::skills`. The
//! agent-curated nudge / self-improvement loop lives in `cersei-agent` and
//! `cersei-hooks`; the registry is just where skills are stored and queried.
//!
//! ## Typical usage
//!
//! ```no_run
//! use cersei_skills::SkillRegistry;
//!
//! let reg = SkillRegistry::from_defaults()?;  // ~/.cersei/skills + .cersei/skills
//! for meta in reg.list() {
//!     println!("{}  — {}", meta.name, meta.description);
//! }
//! if let Some(skill) = reg.view("run-tests") {
//!     println!("{}", skill.body);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! ## File layout
//!
//! ```text
//! ~/.cersei/skills/
//! ├── run-tests/
//! │   ├── SKILL.md          # frontmatter + markdown body
//! │   ├── references/       # supporting docs loaded on demand via view_with_assets
//! │   ├── scripts/
//! │   └── templates/
//! └── deploy-to-vercel/
//!     └── SKILL.md
//! ```
//!
//! ## Design notes
//!
//! 1. **Progressive disclosure.** `list()` returns only `SkillMeta`
//!    (name/description/tags/platforms) — token-cheap, safe to stream into
//!    a system prompt on every turn. `view()` loads the full SKILL.md only
//!    when an agent explicitly requests it.
//!
//! 2. **Name de-dup.** Project-local skills (`.cersei/skills/`) win over
//!    user-global (`~/.cersei/skills/`) when names collide. Same as how
//!    `cargo` handles local vs. global config.
//!
//! 3. **Frontmatter is validated at parse time.** Missing required fields
//!    produce `Err` before the skill enters the registry.
//!
//! 4. **Security is not enforced here.** The `cersei-tools::skills`
//!    adapter gates `SkillManage` behind a `PermissionLevel::Execute`
//!    check. The parser only refuses to load malformed frontmatter.

pub mod parser;
pub mod registry;
pub mod security;

pub use parser::{parse_skill, Skill, SkillFrontmatter, SkillMeta, SkillParseError, SkillPrerequisites};
pub use registry::{RegistrySource, SkillRegistry};
pub use security::{SecurityScan, SkillSecurityIssue};
