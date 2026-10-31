//! Rendering and validation of generated manifest drafts.

use super::*;

// ---------------------------------------------------------------- rendering

fn qp(p: &Path) -> String {
    quote(&p.to_string_lossy())
}

fn list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

impl Draft {
    /// The manifest text. Guaranteed to parse; see [`Draft::validate`].
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("# {}\n", msg!("draft.header.drafted_by")));
        o.push_str(&format!("# {}\n", msg!("draft.header.todo_lines")));
        o.push_str(&format!("# {}\n", msg!("draft.header.reference")));
        o.push_str("schema_version = 1\n\n");

        o.push_str("[project]\n");
        o.push_str(&format!("name = {}\n", quote(&self.name)));
        match self.ros_distro {
            Some(d) => o.push_str(&format!(
                "ros_distro = {:<24} # humble | jazzy | rolling\n",
                quote(d.as_str())
            )),
            None => o.push_str(&format!(
                "ros_distro = \"jazzy\"                  # TODO: {}; humble | jazzy | rolling\n",
                msg!("draft.not_detected")
            )),
        }
        let platforms: Vec<String> = self.platforms.iter().map(ToString::to_string).collect();
        o.push_str(&format!(
            "platforms = {:<25} # {}\n\n",
            list(&platforms),
            msg!("draft.platforms_hint")
        ));

        o.push_str("[environment]\n");
        o.push_str(&format!(
            "provider = {:<26} # native | pixi | docker\n\n",
            quote(self.provider.as_str())
        ));
        match self.provider {
            ProviderKind::Pixi => {
                o.push_str("[environment.pixi]\n");
                o.push_str(&format!(
                    "manifest = {}\n\n",
                    qp(self
                        .pixi_manifest
                        .as_deref()
                        .unwrap_or(Path::new("pixi.toml")))
                ));
            }
            ProviderKind::Native => {
                o.push_str("[environment.native]\n");
                match &self.ubuntu {
                    Some(u) => o.push_str(&format!("ubuntu = {}\n", quote(u))),
                    None => o.push_str(&format!(
                        "# ubuntu = \"24.04\"                     # TODO: {}; {}\n",
                        msg!("draft.not_detected"),
                        msg!("draft.ubuntu_hint")
                    )),
                }
                match &self.ros_base {
                    Some(b) => o.push_str(&format!("ros_base = {}\n", quote(b))),
                    None => o.push_str(&format!(
                        "# ros_base = \"ros-<distro>-ros-base\"    # TODO: {}; {}\n",
                        msg!("draft.not_detected"),
                        msg!("draft.ros_base_hint")
                    )),
                }
                o.push('\n');
            }
            ProviderKind::Docker => {}
        }

        for ws in &self.workspaces {
            o.push_str(&format!("# {}\n", ws.origin));
            o.push_str(&format!("[workspaces.{}]\n", ws.name));
            o.push_str(&format!("path = {}\n", qp(&ws.path)));
            match &ws.sources {
                Some(s) => o.push_str(&format!("sources = {}\n", qp(s))),
                None => o.push_str(&format!(
                    "# sources = \"main.repos\"              # TODO: {}\n",
                    msg!("draft.sources_hint")
                )),
            }
            if ws.underlays.is_empty()
                && self.workspaces.len() > 1
                && self.workspaces.iter().all(|w| w.underlays.is_empty())
            {
                o.push_str(&format!(
                    "underlays = []                        # TODO: {}\n",
                    msg!("draft.underlays_hint")
                ));
            } else {
                o.push_str(&format!("underlays = {}\n", list(&ws.underlays)));
            }
            o.push('\n');
            o.push_str(&format!("[workspaces.{}.colcon]\n", ws.name));
            if let Some(f) = &ws.colcon_defaults {
                o.push_str(&format!("defaults = {}\n", qp(f)));
            }
            o.push_str(&format!(
                "mixins = []                           # TODO: {}\n",
                msg!("draft.mixins_hint")
            ));
            o.push_str("build_args = [\"--symlink-install\"]\n\n");
        }

        o.push_str("[dependencies]\n");
        o.push_str(&format!(
            "apt = []                              # TODO: {}\n",
            msg!("draft.apt_hint")
        ));
        o.push_str(&format!(
            "pip = []                              # {}\n",
            msg!("draft.pip_hint")
        ));
        if !self.external_keys.is_empty() {
            o.push_str(&format!(
                "# {}\n",
                msg!("draft.rosdep_keys", count = self.external_keys.len())
            ));
            for chunk in self.external_keys.chunks(8) {
                o.push_str(&format!("#   {}\n", chunk.join(" ")));
            }
        }
        o.push('\n');
        o.push_str("[dependencies.rosdep]\n");
        o.push_str("enabled = true\n");
        o.push_str("skip_keys = []\n\n");

        o.push_str("[env]\n");
        if self.env.is_empty() {
            o.push_str(&format!(
                "# ROS_DOMAIN_ID = \"0\"                  # TODO: {}\n",
                msg!("draft.env_hint")
            ));
        }
        for (k, v) in &self.env {
            o.push_str(&format!("{k} = {}\n", quote(v)));
        }
        o.push('\n');

        o.push_str("[editor]\n");
        o.push_str(&format!("vscode = {}\n", self.vscode));
        o.push_str(&format!("clion = {}\n", self.clion));
        o.push_str(&format!("devcontainer = {}\n", self.devcontainer));
        o
    }

    /// Parse the rendered text with the real validator.
    pub fn validate(&self) -> crate::error::Result<Manifest> {
        Manifest::parse(&self.render(), Path::new("<init --from-env>"))
    }
}
