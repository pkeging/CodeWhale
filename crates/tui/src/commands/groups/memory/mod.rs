//! Memory command area: persistent memory, quick notes, and user profile.

// This group dir intentionally has a `memory.rs` child module with the same
// name. The module_inception allow is a permanent structure rationale, not
// migration scaffolding; see docs/architecture/command-dispatch.md.
#[allow(clippy::module_inception)]
mod memory;
mod note;
mod profile;

use crate::commands::traits::{Command, CommandGroup, FunctionCommand, RegisterCommand};

pub struct MemoryCommands;

impl CommandGroup for MemoryCommands {
    fn commands(&self) -> Vec<Box<dyn Command>> {
        vec![
            Box::new(FunctionCommand::new(
                note::NoteCmd::info(),
                note::NoteCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                memory::MemoryCmd::info(),
                memory::MemoryCmd::execute,
            )),
            Box::new(FunctionCommand::new(
                profile::ProfileCmd::info(),
                profile::ProfileCmd::execute,
            )),
        ]
    }
}
