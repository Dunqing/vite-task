//! The checkout path an entry was recorded at, and whether its outputs are
//! tied to it.
//!
//! Some tools write the absolute path of the workspace into their outputs:
//! `prettier` and `eslint` key their caches by it, Vite's `_metadata.json`
//! and many source maps name it. Restoring such outputs at another checkout
//! path hands the tool stale paths. So when outputs are archived their bytes
//! are scanned for the root, and an entry whose outputs contain it is shared
//! only between checkouts at the same path. Outputs that hash the path into
//! names or content slip through the scan; that is a documented limit.

use std::io::{self, Read};

use memchr::memmem::Finder;
use serde::{Deserialize, Serialize};
use vt_path::AbsolutePath;
use vt_str::Str;
use wincode::{SchemaRead, SchemaWrite};

use super::FingerprintMismatch;

/// Where an entry was recorded, and whether its outputs are tied to it.
#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead, Serialize, Deserialize)]
pub struct RecordedCheckout {
    /// The workspace root the entry was recorded at, in its displayed form.
    pub root: Str,
    /// Whether any archived output contained `root`.
    pub outputs_contain_root: bool,
}

impl RecordedCheckout {
    /// The recorded form of a workspace root: on Windows without the
    /// verbatim `\\?\` prefix, which tools never write.
    #[must_use]
    pub fn root_string(workspace_root: &AbsolutePath) -> Str {
        Str::from(dunce::simplified(workspace_root.as_path()).to_string_lossy().as_ref())
    }

    /// Why an entry cannot be reused at `workspace_root`, if its outputs
    /// embed the root it was recorded at and that is another one.
    #[must_use]
    pub fn mismatch(&self, workspace_root: &AbsolutePath) -> Option<FingerprintMismatch> {
        (self.outputs_contain_root && self.root != Self::root_string(workspace_root))
            .then(|| FingerprintMismatch::CheckoutPath { recorded_root: self.root.clone() })
    }
}

/// The byte sequences an output carries a workspace root as, for
/// [`RootScanner`].
pub struct RootNeedles {
    finders: Vec<Finder<'static>>,
    longest: usize,
}

impl RootNeedles {
    /// The forms of `workspace_root`: as displayed, with forward slashes,
    /// and on Windows with doubled backslashes, as a JSON or JavaScript
    /// string escapes it. Windows paths are matched without regard to ASCII
    /// case.
    #[must_use]
    pub fn new(workspace_root: &AbsolutePath) -> Self {
        let root = RecordedCheckout::root_string(workspace_root);
        let mut forms: Vec<Vec<u8>> = vec![root.as_bytes().to_vec()];
        if cfg!(windows) {
            forms
                .push(root.as_bytes().iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect());
            let mut escaped = Vec::with_capacity(root.len() * 2);
            for &b in root.as_bytes() {
                escaped.push(b);
                if b == b'\\' {
                    escaped.push(b'\\');
                }
            }
            forms.push(escaped);
            for form in &mut forms {
                form.make_ascii_lowercase();
            }
        }
        forms.sort();
        forms.dedup();
        let longest = forms.iter().map(Vec::len).max().unwrap_or(0);
        let finders = forms.into_iter().map(|form| Finder::new(&form).into_owned()).collect();
        Self { finders, longest }
    }

    fn found_in(&self, haystack: &[u8]) -> bool {
        if cfg!(windows) {
            let lowered = haystack.to_ascii_lowercase();
            self.finders.iter().any(|finder| finder.find(&lowered).is_some())
        } else {
            self.finders.iter().any(|finder| finder.find(haystack).is_some())
        }
    }
}

/// Passes bytes through from `inner` while noting whether any of the
/// needles appeared, across read boundaries.
pub struct RootScanner<'a, R> {
    inner: R,
    needles: &'a RootNeedles,
    /// The last `longest - 1` bytes seen, to match a needle split between
    /// two reads.
    carry: Vec<u8>,
    found: bool,
}

impl<'a, R: Read> RootScanner<'a, R> {
    pub const fn new(inner: R, needles: &'a RootNeedles) -> Self {
        Self { inner, needles, carry: Vec::new(), found: false }
    }

    /// Whether a needle appeared in the bytes read so far.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.found
    }

    fn scan(&mut self, chunk: &[u8]) {
        if self.found || self.needles.longest == 0 {
            return;
        }
        let overlap = self.needles.longest - 1;
        if !self.carry.is_empty() {
            let mut window = self.carry.clone();
            window.extend_from_slice(&chunk[..chunk.len().min(overlap)]);
            if self.needles.found_in(&window) {
                self.found = true;
                return;
            }
        }
        if self.needles.found_in(chunk) {
            self.found = true;
            return;
        }
        self.carry.extend_from_slice(chunk);
        if self.carry.len() > overlap {
            self.carry.drain(..self.carry.len() - overlap);
        }
    }
}

impl<R: Read> Read for RootScanner<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.scan(&buf[..read]);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static AbsolutePath {
        AbsolutePath::new(if cfg!(windows) { r"\\?\C:\work\app" } else { "/work/app" }).unwrap()
    }

    /// Reads `content` a few bytes at a time, so needles straddle reads.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(3).min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    fn scan(content: &[u8]) -> bool {
        let needles = RootNeedles::new(root());
        let mut scanner = RootScanner::new(Trickle(content), &needles);
        io::copy(&mut scanner, &mut io::sink()).unwrap();
        scanner.found()
    }

    #[test]
    fn finds_the_root_across_read_boundaries() {
        let root = RecordedCheckout::root_string(root());
        assert!(scan(vt_str::format!("cache: {root}/src/a.js").as_bytes()));
        assert!(scan(root.as_bytes()));
        assert!(!scan(root[..root.len() - 1].as_bytes()));
        assert!(!scan(b"nothing of the sort"));
        assert!(!scan(b""));
    }

    #[cfg(windows)]
    #[test]
    fn finds_the_root_in_its_escaped_and_forward_slash_forms() {
        assert!(scan(br#"{"C:\\work\\app\\src\\a.js": 1}"#));
        assert!(scan(b"file:///C:/work/app/src/a.js"));
        assert!(scan(b"c:\\WORK\\app"));
        assert!(!scan(b"C:\\work\\other"));
    }

    #[test]
    fn root_is_recorded_without_the_verbatim_prefix() {
        assert_eq!(
            RecordedCheckout::root_string(root()).as_str(),
            if cfg!(windows) { r"C:\work\app" } else { "/work/app" }
        );
    }

    #[test]
    fn entries_whose_outputs_embed_the_root_are_bound_to_it() {
        let other =
            AbsolutePath::new(if cfg!(windows) { r"C:\ci\app" } else { "/ci/app" }).unwrap();
        let bound = RecordedCheckout {
            root: RecordedCheckout::root_string(root()),
            outputs_contain_root: true,
        };
        assert!(bound.mismatch(root()).is_none());
        assert!(matches!(
            bound.mismatch(other),
            Some(FingerprintMismatch::CheckoutPath { recorded_root }) if recorded_root == bound.root
        ));
        let free = RecordedCheckout { outputs_contain_root: false, ..bound };
        assert!(free.mismatch(other).is_none());
    }
}
