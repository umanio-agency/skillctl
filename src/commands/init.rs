use anyhow::Result;

use crate::cli::InitArgs;

pub fn run(args: InitArgs) -> Result<()> {
    println!("init: not implemented yet (url={})", args.url);
    Ok(())
}
