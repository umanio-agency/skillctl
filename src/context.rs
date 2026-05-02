use std::io::IsTerminal;

#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub interactive: bool,
    pub json: bool,
}

impl Context {
    pub fn from_flags(force_non_interactive: bool, json: bool) -> Self {
        let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        Self {
            interactive: !force_non_interactive && !json && tty,
            json,
        }
    }
}
