use std::convert::TryFrom;
use std::io;
use std::path::PathBuf;
use std::process::{self, Command, Stdio};

use dirs::home_dir;
use ini::Ini;
use lazy_static::lazy_static;
use serde_derive::Deserialize;
use structopt::StructOpt;

lazy_static! {
    static ref AWS_CONFIG_PATH: PathBuf = {
        let mut config_path = home_dir().expect("Unable to determine home directory");
        config_path.push(".aws");
        config_path.push("config");
        config_path
    };
}

#[derive(Debug, StructOpt)]
struct App {
    /// The profile to assume.
    #[structopt(required_unless = "role-arn")]
    profile: Option<String>,

    /// The source profile to assume *from*.
    #[structopt(long, conflicts_with = "profile")]
    source_profile: Option<String>,

    /// A specific role ARN to assume.
    #[structopt(long, conflicts_with = "profile", required_unless = "profile")]
    role_arn: Option<String>,

    /// An external ID to use when assuming a specific ARN.
    #[structopt(long, conflicts_with = "profile", requires = "role-arn")]
    external_id: Option<String>,
}

fn main() {
    if let Err(error) = _main() {
        eprintln!("Error: {}", error);
        process::exit(1);
    }
}

fn _main() -> Result<(), AppError> {
    let app = App::from_args();

    let args = AwsArgs::try_from(app)?;
    let mut cmd = Command::new("aws");
    cmd.args(args).stdout(Stdio::piped());

    let child = cmd.spawn()?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        eprintln!();
        return Err(AppError::CmdError(cmd));
    }
    let response = serde_json::from_slice::<CredentialsResponse>(&output.stdout)?;
    println!("{}", response.credentials);

    Ok(())
}

struct AwsArgs {
    source_profile: Option<String>,
    subcommand: AwsSubcommand,
}

enum AwsSubcommand {
    AssumeRole(AssumeRoleArgs),
    GetSessionToken(GetSessionTokenArgs),
}

struct AssumeRoleArgs {
    role_arn: String,
    external_id: Option<String>,
    mfa: Option<(String, String)>,
}

impl AssumeRoleArgs {
    fn new(role_arn: String, external_id: Option<String>, mfa: Option<(String, String)>) -> Self {
        AssumeRoleArgs {
            role_arn,
            external_id,
            mfa,
        }
    }
}

struct GetSessionTokenArgs {
    mfa: Option<(String, String)>,
}

impl GetSessionTokenArgs {
    fn new(mfa: Option<(String, String)>) -> Self {
        GetSessionTokenArgs { mfa }
    }
}

impl TryFrom<App> for AwsArgs {
    type Error = AppError;

    fn try_from(app: App) -> Result<Self, Self::Error> {
        if let Some(name) = app.profile {
            let profile = Ini::load_from_file(&(*AWS_CONFIG_PATH))?
                .delete(Some(format!("profile {}", name)))
                .ok_or_else(|| {
                    format!("profile \"{}\" not found in {:?}", name, *AWS_CONFIG_PATH)
                })?;
            AwsArgs::try_from(profile)
        } else {
            Ok(AwsArgs {
                source_profile: app.source_profile,
                subcommand: AwsSubcommand::AssumeRole(AssumeRoleArgs::new(
                    app.role_arn.unwrap(),
                    app.external_id,
                    None,
                )),
            })
        }
    }
}

impl TryFrom<ini::ini::Properties> for AwsArgs {
    type Error = AppError;

    fn try_from(mut properties: ini::ini::Properties) -> Result<Self, Self::Error> {
        let source_profile = properties.remove("source_profile");
        let role_arn = properties.remove("role_arn");
        let mfa = properties
            .remove("mfa_serial")
            .map(|mfa_serial| -> Result<_, io::Error> {
                eprint!("MFA token: ");
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                Ok((mfa_serial, input.trim().to_string()))
            })
            .transpose()?;

        Ok(match role_arn {
            Some(role_arn) => AwsArgs {
                source_profile,
                subcommand: AwsSubcommand::AssumeRole(AssumeRoleArgs::new(role_arn, None, mfa)),
            },
            None => AwsArgs {
                source_profile,
                subcommand: AwsSubcommand::GetSessionToken(GetSessionTokenArgs::new(mfa)),
            },
        })
    }
}

#[derive(Default)]
struct ArgsBuilder(Vec<String>);

impl ArgsBuilder {
    fn push(&mut self, arg: &str) {
        self.0.push(arg.to_string());
    }

    fn push_flag(&mut self, flag: &str, value: String) {
        self.0.push(flag.to_string());
        self.0.push(value);
    }

    fn push_flag_opt(&mut self, flag: &str, value: Option<String>) {
        if let Some(value) = value {
            self.push_flag(flag, value);
        }
    }
}

trait ExtendArgs {
    fn extend(self, args: &mut ArgsBuilder);
}

impl IntoIterator for AwsArgs {
    type Item = String;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        let mut args = ArgsBuilder::default();
        self.extend(&mut args);
        args.0.into_iter()
    }
}

impl ExtendArgs for AwsArgs {
    fn extend(self, args: &mut ArgsBuilder) {
        args.push_flag_opt("--profile", self.source_profile);
        args.push("sts");
        self.subcommand.extend(args);
    }
}

impl ExtendArgs for AwsSubcommand {
    fn extend(self, args: &mut ArgsBuilder) {
        match self {
            AwsSubcommand::AssumeRole(a) => a.extend(args),
            AwsSubcommand::GetSessionToken(a) => a.extend(args),
        }
    }
}

impl ExtendArgs for AssumeRoleArgs {
    fn extend(self, args: &mut ArgsBuilder) {
        args.push("assume-role");
        args.push_flag("--role-arn", self.role_arn);
        args.push_flag("--role-session-name", "blah".to_string());
        args.push_flag_opt("--external_id", self.external_id);
        if let Some((mfa_serial, mfa_token)) = self.mfa {
            args.push_flag("--serial-number", mfa_serial);
            args.push_flag("--token-code", mfa_token);
        }
    }
}

impl ExtendArgs for GetSessionTokenArgs {
    fn extend(self, args: &mut ArgsBuilder) {
        args.push("get-session-token");
        if let Some((mfa_serial, mfa_token)) = self.mfa {
            args.push_flag("--serial-number", mfa_serial);
            args.push_flag("--token-code", mfa_token);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialsResponse {
    credentials: SessionCredentials,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SessionCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
}

impl std::fmt::Display for SessionCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "export AWS_ACCESS_KEY_ID='{}'
export AWS_SECRET_ACCESS_KEY='{}'
export AWS_SESSION_TOKEN='{}'",
            self.access_key_id, self.secret_access_key, self.session_token
        )
    }
}

#[derive(Debug)]
enum AppError {
    CmdError(Command),
    Generic(String),
    Io(io::Error),
    ProfileError(ini::ini::Error),
    UnexpectedOutput(serde_json::Error),
}

impl From<String> for AppError {
    fn from(error: String) -> Self {
        AppError::Generic(error)
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        AppError::Io(error)
    }
}

impl From<ini::ini::Error> for AppError {
    fn from(error: ini::ini::Error) -> Self {
        AppError::ProfileError(error)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        AppError::UnexpectedOutput(error)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AppError::CmdError(cmd) => write!(f, "AWS CLI call failed: {:?}", cmd),
            AppError::Generic(message) => write!(f, "{}", message),
            AppError::Io(error) => write!(f, "{}", error),
            AppError::ProfileError(error) => {
                write!(f, "unable to read {:?}: {}", *AWS_CONFIG_PATH, error)
            }
            AppError::UnexpectedOutput(error) => {
                write!(f, "unexpected output from AWS CLI call: {}", error)
            }
        }
    }
}
