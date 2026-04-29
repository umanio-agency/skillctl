use anyhow::Result;

use crate::cli::PushArgs;

pub fn run(args: PushArgs) -> Result<()> {
    println!("push: not implemented yet (skills={:?})", args.skills);
    Ok(())
}
