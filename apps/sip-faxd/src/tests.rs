use super::*;
use clap::CommandFactory;

#[test]
fn cli_definition_is_valid() {
    Cli::command().debug_assert();
}
