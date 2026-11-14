use zed_extension_api::{self as zed, Command, LanguageServerId, Worktree, settings::LspSettings};

mod contract;

struct NeutronExtension;

fn server_arguments(configured: Option<Vec<String>>) -> Vec<String> {
    configured.unwrap_or_else(|| {
        contract::SERVER_ARGUMENTS
            .iter()
            .map(|argument| (*argument).to_string())
            .collect()
    })
}

impl zed::Extension for NeutronExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        let (configured_path, configured_arguments, env) = match settings.binary {
            Some(binary) => (
                binary.path,
                binary.arguments,
                binary.env.unwrap_or_default().into_iter().collect(),
            ),
            None => (None, None, Vec::new()),
        };
        let path = configured_path
            .or_else(|| worktree.which(contract::SERVER_COMMAND))
            .ok_or_else(|| {
                "Install NTSC on Zed's PATH, or configure lsp.ntsc.binary.path.".to_string()
            })?;
        Ok(Command {
            command: path,
            args: server_arguments(configured_arguments),
            env,
        })
    }
}

zed::register_extension!(NeutronExtension);

#[cfg(test)]
mod tests {
    use super::server_arguments;

    #[test]
    fn default_uses_the_shared_compiler_subcommand() {
        assert_eq!(server_arguments(None), vec!["lsp"]);
    }

    #[test]
    fn standalone_server_preserves_explicit_empty_arguments() {
        assert!(server_arguments(Some(Vec::new())).is_empty());
    }

    #[test]
    fn configured_arguments_are_not_replaced() {
        assert_eq!(
            server_arguments(Some(vec!["custom".to_string()])),
            vec!["custom"]
        );
    }
}
