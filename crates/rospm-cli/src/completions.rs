//! Shell completion scripts use the same command definitions as help and parsing.

use std::io::{self, Write};

use clap::{Command, CommandFactory};
use clap_complete::Shell;

pub(crate) fn script(shell: Shell) -> Vec<u8> {
    let mut command = public_command(&crate::Cli::command());
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut command, "rospm", &mut script);
    script
}

fn public_command(command: &Command) -> Command {
    // Static generators also include hidden entries. Keep worker internals out
    // of completion menus while reusing the public arguments and their values.
    let mut subcommands: Vec<_> = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(public_command)
        .collect();
    subcommands.sort_by(|a, b| a.get_name().cmp(b.get_name()));
    let mut public = Command::new(command.get_name().to_owned())
        .about(command.get_about().cloned().unwrap_or_default())
        .visible_aliases(command.get_visible_aliases().map(str::to_owned))
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .args(
            command
                .get_arguments()
                .filter(|arg| !arg.is_hide_set())
                .cloned(),
        )
        .subcommands(subcommands);
    if let Some(version) = command.get_version() {
        public = public.version(version.to_owned());
    }
    public
}

pub(crate) fn print(shell: Shell) -> anyhow::Result<()> {
    match io::stdout().lock().write_all(&script(shell)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}
