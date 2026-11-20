//! Which build the bridge reaches, and where that build keeps its channel files.
//!
//! The client's config always passes `--build release|dev` and `--app-data-dir <dir>` (the app writes both into the
//! config it copies); the bridge still works without `--app-data-dir` (the platform data directory plus the build's
//! folder name) but never guesses further: it reads only that build's own `mcp/{token,endpoint,enabled,tools.json}`.

use std::path::PathBuf;

/// The release build's folder name under the platform data directory. Core's `paths::APP_DATA_DIR_NAME_RELEASE` is the
/// same text (a test pins it).
const DIR_RELEASE: &str = "com.vsharifov.athenaeum";
/// The debug build's folder name (`paths::APP_DATA_DIR_NAME_DEV` in core).
const DIR_DEV: &str = "com.vsharifov.athenaeum.dev";

/// The usage line every argument error ends with.
pub const USAGE: &str = "usage: athenaeum-mcp --build release|dev [--app-data-dir <dir>]";

/// Which build of Athenaeum this bridge is set up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Release,
    Dev,
}

impl Build {
    /// The build's data folder name.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Release => DIR_RELEASE,
            Self::Dev => DIR_DEV,
        }
    }

    /// The name the build's host introduces itself with (`OK <server name> <version>`): `athenaeum` or `athenaeum-dev`.
    pub fn server_name(self) -> &'static str {
        match self {
            Self::Release => "athenaeum",
            Self::Dev => "athenaeum-dev",
        }
    }
}

/// What the client's config told the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOptions {
    pub build: Build,
    /// `--app-data-dir`: the build's data folder, when the config names it.
    pub app_data_dir: Option<PathBuf>,
}

/// Reads the arguments after the program name: `--build release|dev` (required), `--app-data-dir <dir>` (optional);
/// both also as `--flag=value`. The error is the sentence for the person who edits the client's config.
pub fn parse_args(args: impl Iterator<Item = String>) -> Result<BridgeOptions, String> {
    let mut build = None;
    let mut app_data_dir = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg, None),
        };
        match flag.as_str() {
            "--build" => {
                let value = inline.or_else(|| args.next()).ok_or_else(|| format!("--build needs a value: --build release|dev. {USAGE}"))?;
                build = Some(match value.as_str() {
                    "release" => Build::Release,
                    "dev" => Build::Dev,
                    other => return Err(format!("unknown build '{other}': --build release|dev. {USAGE}")),
                });
            }
            "--app-data-dir" => {
                let value = inline.or_else(|| args.next()).filter(|v| !v.is_empty());
                app_data_dir = Some(PathBuf::from(value.ok_or_else(|| format!("--app-data-dir needs a folder. {USAGE}"))?));
            }
            other => return Err(format!("unknown argument '{other}'. {USAGE}")),
        }
    }
    let build = build.ok_or_else(|| format!("missing --build release|dev. {USAGE}"))?;
    Ok(BridgeOptions { build, app_data_dir })
}

/// The build's data folder: the one the config names, else the platform data directory plus the build's folder name.
/// `None` when neither is known.
pub fn app_data_dir(opts: &BridgeOptions) -> Option<PathBuf> {
    match &opts.app_data_dir {
        Some(dir) => Some(dir.clone()),
        None => dirs::data_dir().map(|base| base.join(opts.build.dir_name())),
    }
}
