//! Whether an edited file is scratch (rule R5, docs/hooks.md sections 3.4 and 4).
//!
//! An edit is scratch when the file is under a scratch place:
//!
//! - the operating system's temporary folder ([`std::env::temp_dir`]);
//! - a hidden top-level folder of the user's home directory (`~/.<name>/...`, e.g.
//!   `~/.claude/`). Home is `USERPROFILE`, else `HOME` (as for the config file).
//!
//! Exception: when the board's project folder (its stored `cwd`) is itself inside that same
//! scratch place and the file is inside the project folder, the edit is not scratch. A
//! project folder that merely contains the scratch place (e.g. the home directory) does not
//! rescue the file.
//!
//! The comparison is on paths, without touching the file system first:
//!
//! - a relative path is resolved against the hook input's `cwd`;
//! - `.` and `..` are removed lexically;
//! - separators follow the platform (`/` and `\` on Windows, `/` elsewhere);
//! - letter case is ignored on Windows and macOS (their default file systems ignore it),
//!   and compared exactly elsewhere;
//! - the spellings of the same Windows drive (`C:` and `\\?\C:`) are the same.
//!
//! Then every path is resolved through symbolic links (macOS `/var` vs `/private/var`) and
//! compared again; a path that does not exist yet is resolved through its nearest existing
//! folder, followed by the rest of its components. The file is scratch when either comparison
//! puts it in a scratch place and neither comparison grants the exception. Every case that
//! cannot be decided is not scratch, so R5 never loses an edit to doubt: no path, a relative
//! path without a hook `cwd`, a path that stays relative.

use std::path::{Component, Path, PathBuf, Prefix};

/// The scratch places of a machine: the temporary folder and the home directory (whose
/// hidden top-level folders are scratch). A place that is missing, relative or a bare root
/// is ignored.
#[derive(Debug, Clone, Default)]
pub struct Places {
    pub temp: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Places {
    /// This process's places: [`std::env::temp_dir`] and `USERPROFILE`, else `HOME`.
    pub fn current() -> Places {
        Places {
            temp: Some(std::env::temp_dir()),
            home: crate::config::home_dir(),
        }
    }
}

/// Whether `path` (a file a tool edited) is scratch on this machine. `project` is the
/// board's `cwd`; `cwd` is the hook input's working folder, used for a relative `path`.
/// Undecidable cases are not scratch (see the module documentation).
pub fn is_scratch(project: Option<&str>, cwd: Option<&str>, path: &str) -> bool {
    is_scratch_in(&Places::current(), project, cwd, path)
}

/// [`is_scratch`] with the given places.
pub fn is_scratch_in(places: &Places, project: Option<&str>, cwd: Option<&str>, path: &str) -> bool {
    is_scratch_with(places, project, cwd, path, &|p| {
        std::fs::canonicalize(p).ok()
    })
}

/// [`is_scratch_in`] with the given way of resolving an existing path through symbolic links
/// (`None` when the path does not exist).
fn is_scratch_with(
    places: &Places,
    project: Option<&str>,
    cwd: Option<&str>,
    path: &str,
    canonicalize: &dyn Fn(&Path) -> Option<PathBuf>,
) -> bool {
    let Some(file) = absolute_file(cwd, path) else {
        return false;
    };
    let project = non_empty(project)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let temp = places.temp.as_deref().filter(|p| usable_root(p));
    let home = places.home.as_deref().filter(|p| usable_root(p));

    let literal = View {
        file: key(&file),
        project: project.as_deref().map(key),
        temp: temp.map(key),
        home: home.map(key),
    };
    let resolved_key = |p: &Path| resolve(p, canonicalize).map(|p| key(&p));
    let resolved = resolved_key(&file).map(|f| View {
        file: f,
        project: project.as_deref().and_then(resolved_key),
        temp: temp.and_then(resolved_key),
        home: home.and_then(resolved_key),
    });
    let views: Vec<&View> = std::iter::once(&literal).chain(resolved.as_ref()).collect();
    let mut in_place = false;
    for v in views {
        if let Some(place) = v.place() {
            in_place = true;
            if v.project_rescues(&place) {
                return false;
            }
        }
    }
    in_place
}

/// One way of looking at the paths: literal or resolved through symbolic links.
struct View {
    file: Vec<String>,
    project: Option<Vec<String>>,
    temp: Option<Vec<String>>,
    home: Option<Vec<String>>,
}

impl View {
    /// The scratch place the file is in: the temporary folder, or the hidden top-level
    /// folder of home that holds it (a hidden file directly in home is not in one).
    fn place(&self) -> Option<Vec<String>> {
        if let Some(temp) = &self.temp
            && within(&self.file, temp)
        {
            return Some(temp.clone());
        }
        let home = self.home.as_ref()?;
        if self.file.len() > home.len() + 1
            && within(&self.file, home)
            && self.file[home.len()].starts_with('.')
        {
            return Some(self.file[..=home.len()].to_vec());
        }
        None
    }

    /// The project folder is inside `place` and the file is inside the project folder.
    fn project_rescues(&self, place: &[String]) -> bool {
        self.project
            .as_ref()
            .is_some_and(|p| within(p, place) && within(&self.file, p))
    }
}

/// The edited file as an absolute path; `None` when it cannot be decided.
fn absolute_file(cwd: Option<&str>, path: &str) -> Option<PathBuf> {
    let file = PathBuf::from(non_empty(Some(path))?);
    if file.is_absolute() {
        return Some(file);
    }
    // Windows: `\dir\file` (current drive) or `/c/dir` (a Unix-style shell path) has a root
    // but no drive; it cannot be resolved reliably.
    if file.has_root() {
        return None;
    }
    let file = Path::new(non_empty(cwd)?).join(file);
    file.is_absolute().then_some(file)
}

/// An absolute path with at least one folder name (not `/` or `C:\` alone).
fn usable_root(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .any(|c| matches!(c, Component::Normal(_)))
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// An absolute path resolved through symbolic links: its longest existing part resolved,
/// followed by the rest of its components (they do not exist, so they are not links). A
/// file or folder that does not exist yet thus resolves through its nearest existing
/// folder. `None` when no part of it exists.
fn resolve(path: &Path, canonicalize: &dyn Fn(&Path) -> Option<PathBuf>) -> Option<PathBuf> {
    path.ancestors().find_map(|existing| {
        let real = canonicalize(existing)?;
        let rest = path.strip_prefix(existing).ok()?;
        Some(if rest.as_os_str().is_empty() {
            real
        } else {
            real.join(rest)
        })
    })
}

/// `file` is `root` or below it.
fn within(file: &[String], root: &[String]) -> bool {
    file.len() >= root.len() && file[..root.len()] == *root
}

/// The comparable form of an absolute path: its components after removing `.` and `..`,
/// with the case folded where the platform ignores it.
fn key(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The prefix and the root stay: `..` never climbs above them.
    let mut fixed = 0;
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                out.push(prefix_key(p.kind()));
                fixed = out.len();
            }
            Component::RootDir => {
                out.push("/".to_string());
                fixed = out.len();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if out.len() > fixed {
                    out.pop();
                }
            }
            Component::Normal(s) => out.push(fold(&s.to_string_lossy())),
        }
    }
    out
}

/// One form for the spellings of the same Windows prefix (`C:` and `\\?\C:`; `\\server\share`
/// and `\\?\UNC\server\share`).
fn prefix_key(prefix: Prefix<'_>) -> String {
    match prefix {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
            format!("{}:", (d as char).to_ascii_lowercase())
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
            "//{}/{}",
            fold(&server.to_string_lossy()),
            fold(&share.to_string_lossy())
        ),
        Prefix::Verbatim(s) => format!("//?/{}", fold(&s.to_string_lossy())),
        Prefix::DeviceNS(s) => format!("//./{}", fold(&s.to_string_lossy())),
    }
}

/// Case folding where the platform's default file system ignores case.
fn fold(s: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const TEMP: &str = r"C:\Users\u\AppData\Local\Temp";
    #[cfg(not(windows))]
    const TEMP: &str = "/tmp";

    #[cfg(windows)]
    const HOME: &str = r"C:\Users\u";
    #[cfg(not(windows))]
    const HOME: &str = "/home/u";

    #[cfg(windows)]
    const PROJECT: &str = r"D:\Project\tui_kanban";
    #[cfg(not(windows))]
    const PROJECT: &str = "/srv/project/tui_kanban";

    fn places() -> Places {
        Places {
            temp: Some(PathBuf::from(TEMP)),
            home: Some(PathBuf::from(HOME)),
        }
    }

    /// `root` joined with `parts`, as text.
    fn p(root: &str, parts: &[&str]) -> String {
        parts
            .iter()
            .fold(PathBuf::from(root), |acc, part| acc.join(part))
            .to_string_lossy()
            .to_string()
    }

    fn scratch(project: &str, path: &str) -> bool {
        is_scratch_in(&places(), Some(project), Some(project), path)
    }

    #[test]
    fn the_temporary_folder_is_scratch() {
        assert!(scratch(PROJECT, &p(TEMP, &["kb-preview.html"])));
        assert!(scratch(PROJECT, &p(TEMP, &["a", "b", "notes.md"])));
        // A relative path against a hook cwd in the temporary folder.
        assert!(is_scratch_in(&places(), Some(PROJECT), Some(TEMP), "x.md"));
        // A sibling whose name starts with the temporary folder's name is not in it.
        assert!(!scratch(PROJECT, &format!("{TEMP}2{}x.md", std::path::MAIN_SEPARATOR)));
    }

    #[test]
    fn hidden_top_level_folders_of_home_are_scratch() {
        assert!(scratch(PROJECT, &p(HOME, &[".claude", "projects", "memory", "MEMORY.md"])));
        assert!(scratch(PROJECT, &p(HOME, &[".config", "kb", "rules.toml"])));
        // A hidden file directly in home is not in a hidden folder.
        assert!(!scratch(PROJECT, &p(HOME, &[".bashrc"])));
        // A hidden folder deeper down is not a top-level one.
        assert!(!scratch(PROJECT, &p(HOME, &["src", ".cache", "x"])));
        // Another user's home.
        assert!(!scratch(PROJECT, &p(&format!("{HOME}2"), &[".claude", "x.md"])));
    }

    #[test]
    fn a_project_inside_the_same_scratch_place_rescues_its_files() {
        let in_temp = p(TEMP, &["kb-proj"]);
        assert!(!scratch(&in_temp, &p(&in_temp, &["src", "main.rs"])));
        assert!(!scratch(&in_temp, "src/main.rs"));
        // Other files of the temporary folder stay scratch.
        assert!(scratch(&in_temp, &p(TEMP, &["other.md"])));
        // A project in the temporary folder does not rescue a file in ~/.claude.
        assert!(scratch(&in_temp, &p(HOME, &[".claude", "x.md"])));

        let in_claude = p(HOME, &[".claude", "skills", "kb"]);
        assert!(!scratch(&in_claude, &p(&in_claude, &["SKILL.md"])));
        assert!(scratch(&in_claude, &p(HOME, &[".claude", "settings.json"])));
        // Same place means the same hidden folder.
        assert!(scratch(&in_claude, &p(HOME, &[".config", "x.toml"])));
    }

    #[test]
    fn home_as_the_project_does_not_rescue_hidden_folders() {
        assert!(scratch(HOME, &p(HOME, &[".claude", "settings.json"])));
        assert!(!scratch(HOME, &p(HOME, &["src", "x", "main.rs"])));
    }

    #[test]
    fn every_other_edit_counts() {
        assert!(!scratch(PROJECT, &p(PROJECT, &["src", "main.rs"])));
        // Outside the project, elsewhere.
        #[cfg(windows)]
        assert!(!scratch(PROJECT, r"E:\other\project\lib.rs"));
        #[cfg(not(windows))]
        assert!(!scratch(PROJECT, "/opt/other/project/lib.rs"));
        assert!(!scratch(PROJECT, &p(HOME, &["src", "x", "main.rs"])));
    }

    #[test]
    fn undecidable_cases_count() {
        // No path.
        assert!(!scratch(PROJECT, ""));
        // A relative path without a hook cwd.
        assert!(!is_scratch_in(&places(), Some(PROJECT), None, "x.md"));
        // No stored cwd: the places still apply, there is no exception.
        let tmp_file = p(TEMP, &["x.md"]);
        assert!(is_scratch_in(&places(), None, None, &tmp_file));
        assert!(is_scratch_in(&places(), Some("relative"), None, &tmp_file));
        // No places.
        assert!(!is_scratch_in(&Places::default(), Some(PROJECT), None, &tmp_file));
        // A bare root is not a usable place.
        let root = Places {
            temp: Some(PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" })),
            home: None,
        };
        assert!(!is_scratch_in(&root, Some(PROJECT), None, &tmp_file));
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_drive_spellings() {
        assert!(scratch(PROJECT, r"c:\users\U\.Claude\x.md"));
        assert!(scratch(PROJECT, r"\\?\C:\Users\u\AppData\Local\TEMP\x.md"));
        assert!(scratch(PROJECT, "C:/Users/u/.claude/x.md"));
        // A rooted path without a drive cannot be resolved: counts.
        assert!(!scratch(PROJECT, "/c/Users/u/.claude/x.md"));
        assert!(!scratch(PROJECT, r"\Users\u\.claude\x.md"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_compares_case_exactly() {
        assert!(!scratch(PROJECT, "/TMP/x.md"));
        assert!(!scratch(PROJECT, "/home/U/.claude/x.md"));
    }

    #[test]
    fn this_machine_uses_its_temporary_folder() {
        let file = std::env::temp_dir().join("kb-scratch-test.md");
        assert!(is_scratch(Some(PROJECT), None, &file.to_string_lossy()));
    }

    /// A new, empty folder in the temporary folder.
    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kb-project-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_new_file_resolves_through_its_nearest_existing_folder() {
        // The macOS case on any platform, with a simulated file system: `<root>var` is a
        // link to `<root>private/var`, the temporary folder is spelled through the link
        // (TMPDIR), the board's project folder is spelled for real (getcwd), and only the
        // project folder exists below the temporary folder.
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let link = PathBuf::from(p(root, &["var"]));
        let real = PathBuf::from(p(root, &["private", "var"]));
        let temp = p(&link.to_string_lossy(), &["folders", "xx", "T"]);
        let real_temp = p(&real.to_string_lossy(), &["folders", "xx", "T"]);
        let project = p(&real_temp, &["proj"]);
        let hook_cwd = p(&temp, &["proj"]);
        let existing = PathBuf::from(&project);
        let canonicalize = |path: &Path| -> Option<PathBuf> {
            let target = match path.strip_prefix(&link) {
                Ok(rest) if rest.as_os_str().is_empty() => real.clone(),
                Ok(rest) => real.join(rest),
                Err(_) => path.to_path_buf(),
            };
            existing.starts_with(&target).then_some(target)
        };
        let places = Places {
            temp: Some(PathBuf::from(&temp)),
            home: None,
        };
        let s = |path: &str| {
            is_scratch_with(
                &places,
                Some(project.as_str()),
                Some(hook_cwd.as_str()),
                path,
                &canonicalize,
            )
        };
        // Neither `src/main.rs` nor `src/` exists: the project's file still counts.
        assert!(!s("src/main.rs"));
        assert!(!s(&p(&hook_cwd, &["src", "main.rs"])));
        assert!(!s(&p(&project, &["src", "main.rs"])));
        // New files elsewhere in the temporary folder stay scratch, in either spelling.
        assert!(s(&p(&temp, &["scratch-1", "notes.md"])));
        assert!(s(&p(&real_temp, &["scratch-1", "notes.md"])));
        assert!(s("../scratch-1/notes.md"));
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_of_a_project_reached_through_a_real_link_counts() {
        // The macOS case with a real link: the temporary folder is spelled through the link,
        // the board's project folder for real, and the edited file and its folder do not
        // exist yet.
        let base = fresh_dir("new-file");
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(real.join("proj")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let places = Places {
            temp: Some(link.clone()),
            home: None,
        };
        let project = std::fs::canonicalize(real.join("proj")).unwrap();
        let project = project.to_string_lossy().to_string();
        let hook_cwd = link.join("proj").to_string_lossy().to_string();
        let s = |path: &str| {
            is_scratch_in(
                &places,
                Some(project.as_str()),
                Some(hook_cwd.as_str()),
                path,
            )
        };
        let project_file = s("src/main.rs");
        let other_file = s(&link.join("new").join("notes.md").to_string_lossy());
        let _ = std::fs::remove_dir_all(&base);
        assert!(!project_file);
        assert!(other_file);
    }

    #[test]
    fn links_are_resolved() {
        // A scratch place reached through a symbolic link (as macOS /var is /private/var),
        // when the platform lets the test create one (Windows needs a privilege for it).
        let base = fresh_dir("links");
        let real = base.join("real");
        std::fs::create_dir_all(real.join("proj")).unwrap();
        std::fs::write(real.join("a.txt"), "x").unwrap();
        std::fs::write(real.join("proj").join("b.txt"), "x").unwrap();
        let link = base.join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&real, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &link).is_ok();
        if made {
            let places = Places {
                temp: Some(link.clone()),
                home: None,
            };
            let s = |project: &Path, file: &Path| {
                is_scratch_in(
                    &places,
                    Some(&project.to_string_lossy()),
                    None,
                    &file.to_string_lossy(),
                )
            };
            let elsewhere = Path::new(PROJECT);
            // The real path of a file in the place: scratch.
            assert!(s(elsewhere, &real.join("a.txt")));
            // A file that does not exist (yet).
            assert!(s(elsewhere, &real.join("new.txt")));
            // A project in the place spelled through the link, its file spelled for real.
            assert!(!s(&link.join("proj"), &real.join("proj").join("b.txt")));
            assert!(!s(&real.join("proj"), &link.join("proj").join("b.txt")));
            // A file of the project in a folder that does not exist yet.
            let new = link.join("proj").join("src").join("main.rs");
            assert!(!s(&real.join("proj"), &new));
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
