#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Title,
    Osc7,
}

#[derive(Default)]
pub(crate) struct Reports {
    title: Option<String>,
    osc7: Option<String>,
    selected: Option<Source>,
    changed: Option<Source>,
}

impl Reports {
    pub(super) fn record(&mut self, source: Source, path: String) {
        let previous = match source {
            Source::Title => &mut self.title,
            Source::Osc7 => &mut self.osc7,
        };
        if previous.as_ref() == Some(&path) {
            return;
        }
        *previous = Some(path);
        // Both updates may be split across SSH packets in the same UI event drain.
        if self.changed != Some(Source::Osc7) {
            self.changed = Some(source);
        }
    }

    pub(crate) fn finish_batch(&mut self) -> bool {
        let Some(source) = self.changed.take() else {
            return false;
        };
        self.selected = Some(source);
        true
    }

    pub(super) fn path(&self, home: &str) -> Option<String> {
        match self.selected? {
            Source::Osc7 => self.osc7.clone(),
            Source::Title => {
                let path = self.title.as_deref()?;
                if path == "~" {
                    Some(home.to_owned())
                } else if let Some(rest) = path.strip_prefix("~/") {
                    Some(format!("{}/{rest}", home.trim_end_matches('/')))
                } else {
                    Some(path.to_owned())
                }
            }
        }
    }
}

fn is_drive_path(path: &str) -> bool {
    matches!(
        path.as_bytes(),
        [b'a'..=b'z' | b'A'..=b'Z', b':', b'/' | b'\\', ..]
    )
}

pub(super) fn title_path(title: &str) -> Option<String> {
    let title = title
        .trim()
        .strip_prefix("ASHELL_CWD:")
        .unwrap_or(title.trim());
    let path = if title.starts_with(['/', '~']) || is_drive_path(title) {
        title
    } else {
        title.split_once(':')?.1.trim()
    };
    if path.chars().any(char::is_control) {
        return None;
    }
    if is_drive_path(path) {
        return Some(format!("/{}", path.replace('\\', "/")));
    }
    (path.starts_with('/') || path == "~" || path.starts_with("~/")).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_reports_resolve_home_and_preserve_drive_paths_and_colons() {
        let mut reports = Reports::default();
        for (title, expected) in [
            ("user@server: ~/src", "/home/test/src"),
            ("ASHELL_CWD:/srv/a:b", "/srv/a:b"),
            (r"C:\Users\dev", "/C:/Users/dev"),
        ] {
            reports.record(Source::Title, title_path(title).unwrap());
            reports.finish_batch();
            assert_eq!(reports.path("/home/test").as_deref(), Some(expected));
        }
        assert!(title_path("building project").is_none());
    }
}
