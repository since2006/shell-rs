//! Back / forward directory history for one pane.

/// Why a directory load started; it decides how a successful load changes
/// the history. A failed load changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadIntent {
    /// Going somewhere new: typed path, double-click, up, root, home, bookmark.
    Visit,
    Back,
    Forward,
    /// Refresh, reconnect, or re-reading after an operation.
    Reload,
}

const LIMIT: usize = 50;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavigationHistory {
    back: Vec<String>,
    forward: Vec<String>,
}

impl NavigationHistory {
    pub fn back_target(&self) -> Option<&str> {
        self.back.last().map(String::as_str)
    }

    pub fn forward_target(&self) -> Option<&str> {
        self.forward.last().map(String::as_str)
    }

    /// Record a successful load of `to` that started from `from`.
    pub fn record(&mut self, intent: LoadIntent, from: &str, to: &str) {
        match intent {
            LoadIntent::Visit => {
                if !from.is_empty() && from != to {
                    push(&mut self.back, from);
                    self.forward.clear();
                }
            }
            LoadIntent::Back => {
                self.back.pop();
                push(&mut self.forward, from);
            }
            LoadIntent::Forward => {
                self.forward.pop();
                push(&mut self.back, from);
            }
            LoadIntent::Reload => {}
        }
    }
}

fn push(stack: &mut Vec<String>, path: &str) {
    if path.is_empty() {
        return;
    }
    stack.push(path.to_string());
    if stack.len() > LIMIT {
        stack.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_push_back_and_clear_forward() {
        let mut history = NavigationHistory::default();
        history.record(LoadIntent::Visit, "", "/home");
        assert_eq!(history.back_target(), None, "the first load has no origin");
        history.record(LoadIntent::Visit, "/home", "/etc");
        history.record(LoadIntent::Visit, "/etc", "/etc");
        history.record(LoadIntent::Reload, "/etc", "/etc");
        assert_eq!(history.back_target(), Some("/home"));
        history.record(LoadIntent::Back, "/etc", "/home");
        assert_eq!(history.back_target(), None);
        assert_eq!(history.forward_target(), Some("/etc"));
        history.record(LoadIntent::Forward, "/home", "/etc");
        assert_eq!(history.back_target(), Some("/home"));
        assert_eq!(history.forward_target(), None);
        history.record(LoadIntent::Back, "/etc", "/home");
        history.record(LoadIntent::Visit, "/home", "/var");
        assert_eq!(history.forward_target(), None, "a new visit drops forward");
    }

    #[test]
    fn history_is_bounded() {
        let mut history = NavigationHistory::default();
        for ix in 0..60 {
            history.record(
                LoadIntent::Visit,
                &format!("/{ix}"),
                &format!("/{}", ix + 1),
            );
        }
        assert_eq!(history.back.len(), LIMIT);
        assert_eq!(history.back_target(), Some("/59"));
        assert_eq!(history.back[0], "/10");
    }
}
