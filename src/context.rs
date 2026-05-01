use std::io::IsTerminal;

#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub interactive: bool,
}

impl Context {
    pub fn from_flag(force_non_interactive: bool) -> Self {
        let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        Self {
            interactive: !force_non_interactive && tty,
        }
    }
}
