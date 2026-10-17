//! A package's own `node_modules` inside pnpm's store,
//! `node_modules/.pnpm/<entry>/node_modules/<pkg>/node_modules`, when it
//! holds nothing but `.bin`. pnpm creates that `.bin` only while it runs the
//! package's build script, so a cold store has the directory and a warm
//! store (or another checkout installed from the same store) does not.
//! Module resolution checks the directory on its way up, so without this a
//! build recorded right after a lockfile change misses on the next install.
//!
//! The directory is fingerprinted as missing on both installs. Nothing
//! resolves through it anyway.

use vt_path::AbsolutePath;

/// Whether `path` is a store package's own `node_modules`: the package may
/// be scoped (`@scope/<pkg>`).
pub fn matches(path: &AbsolutePath) -> bool {
    let mut components = path.as_path().components().rev().filter_map(|component| {
        if let std::path::Component::Normal(name) = component { name.to_str() } else { None }
    });
    let mut next = || components.next();
    if next() != Some("node_modules") {
        return false;
    }
    // `<pkg>`, or `<pkg>` then `@scope`.
    let Some(package) = next() else {
        return false;
    };
    if package.starts_with('@') || package == "node_modules" {
        return false;
    }
    let mut parent = next();
    if parent.is_some_and(|name| name.starts_with('@')) {
        parent = next();
    }
    parent == Some("node_modules")
        && next().is_some_and(|entry| entry != ".pnpm" && entry != "node_modules")
        && next() == Some(".pnpm")
        && next() == Some("node_modules")
}

/// Whether the directory at `path` holds nothing but `.bin`. A missing or
/// unreadable directory doesn't.
pub fn holds_only_bin(path: &AbsolutePath) -> bool {
    let Ok(entries) = std::fs::read_dir(path.as_path()) else {
        return false;
    };
    let mut names = entries.map(|entry| entry.map(|entry| entry.file_name()));
    names.all(|name| name.is_ok_and(|name| name == ".bin" || name == ".DS_Store"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_store_package_own_node_modules() {
        let cwd = vt_path::current_dir().unwrap();
        assert!(matches(
            &cwd.join("node_modules/.pnpm/esbuild@0.28.2/node_modules/esbuild/node_modules")
        ));
        assert!(matches(
            &cwd.join("node_modules/.pnpm/@scope+pkg@1.0.0/node_modules/@scope/pkg/node_modules")
        ));
        assert!(matches(&cwd.join(
            "packages/a/node_modules/.pnpm/esbuild@0.28.2_x@1/node_modules/esbuild/node_modules"
        )));
        #[cfg(windows)]
        assert!(matches(
            &cwd.join(r"node_modules\.pnpm\esbuild@0.28.2\node_modules\esbuild\node_modules")
        ));
    }

    #[test]
    fn does_not_match_other_node_modules() {
        let cwd = vt_path::current_dir().unwrap();
        for path in [
            "node_modules",
            "node_modules/esbuild/node_modules",
            "node_modules/.pnpm/esbuild@0.28.2/node_modules",
            "node_modules/.pnpm/esbuild@0.28.2/node_modules/esbuild",
            "node_modules/.pnpm/esbuild@0.28.2/node_modules/esbuild/node_modules/.bin",
            "node_modules/.pnpm/esbuild@0.28.2/node_modules/@scope/node_modules",
            "node_modules/.pnpm/node_modules/esbuild/node_modules",
            ".pnpm/esbuild@0.28.2/node_modules/esbuild/node_modules",
        ] {
            assert!(!matches(&cwd.join(path)), "{path}");
        }
    }

    #[test]
    fn only_bin_means_no_other_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = vt_path::AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        assert!(!holds_only_bin(&dir.join("missing")));
        assert!(holds_only_bin(&dir));
        std::fs::create_dir(dir.join(".bin")).unwrap();
        assert!(holds_only_bin(&dir));
        std::fs::create_dir(dir.join("left-pad")).unwrap();
        assert!(!holds_only_bin(&dir));
    }
}
