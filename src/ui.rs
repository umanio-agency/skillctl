//! Thin wrappers over cliclack that suppress all human-readable output
//! when `Context::json` is true. JSON mode is owned by the caller — it
//! prints a single structured object at the end of the command run.

use std::fmt::Display;

use anyhow::Result;

use crate::context::Context;

pub fn intro<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::intro(msg.to_string())?;
    Ok(())
}

pub fn outro<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::outro(msg)?;
    Ok(())
}

pub fn outro_cancel<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::outro_cancel(msg)?;
    Ok(())
}

pub fn log_info<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::log::info(msg)?;
    Ok(())
}

pub fn log_success<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::log::success(msg)?;
    Ok(())
}

pub fn log_warning<S: Display>(ctx: &Context, msg: S) -> Result<()> {
    if ctx.json {
        return Ok(());
    }
    cliclack::log::warning(msg)?;
    Ok(())
}
