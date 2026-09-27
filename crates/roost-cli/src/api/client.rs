//! The one way `roost api` reaches a coordinator, and the one way it reads what
//! came back. Called by every verb in the module; depends on the generated
//! Connect client in `roost-proto`, on `api::credentials` for where to point,
//! and on nothing else.
//!
//! NOTHING HERE NAMES A METHOD. Every call site writes
//! `api.stub().sessions_list(..)` or `api.stub().agent_status_get(..)`: the
//! generated method, whose spec carries the service and method names. A path
//! typed as a string would be a second hand-maintained copy of a name the
//! generator owns, and the copy is what goes stale — a `.proto` rename would
//! leave the literal compiling and answering 404 at the moment an operator is
//! scripting a fleet change.
//!
//! WHY A COORDINATOR THAT SAYS NOTHING IS NOT AN EMPTY RESULT. Every verb here
//! reads a coordinator that was asked a question; a coordinator that did not
//! answer has answered nothing, and a verb that printed an empty table for it
//! would be indistinguishable from a fleet with nothing in it. The refusal is
//! therefore its own sentence, and it says which of the two happened.

use std::time::Duration;

use axum::http::Uri;
use connectrpc::client::{ClientConfig, HttpClient, UnaryResponse};
use connectrpc::{ConnectError, ErrorCode};
use roost_host::{EnvSource, HostPlatform};
use roost_proto::buffa::view::{MessageView, OwnedView};
use roost_proto::roost::v1::CoordinatorServiceClient;

use crate::api::credentials;
use crate::command_error::CommandFailure;
use crate::deploy::keeper_client::COORD_URL_ENV;

/// How long one coordinator call may take before it counts as unanswered.
///
/// Generous, because the calls behind `agent-wait` and `attach` are the two
/// that legitimately take a while: a wait is bounded by its own `--timeout`,
/// which the coordinator enforces, and an upload is bounded by its size. This
/// is a bound on this process's patience, not on the work.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// A coordinator this command can address.
pub struct CoordinatorApi {
    origin: String,
    client: CoordinatorServiceClient<HttpClient>,
}

impl CoordinatorApi {
    /// The coordinator this shell and this machine can name, presenting
    /// whatever bearer the operator enrolled.
    pub fn from_environment(
        env: &dyn EnvSource,
        platform: HostPlatform,
    ) -> Result<Self, CommandFailure> {
        let origin = credentials::origin(env, platform)?;
        Self::at(&origin, credentials::token(env).as_deref())
    }

    /// A coordinator at a named origin, presenting a named bearer.
    ///
    /// Public because it is the seam a test uses to point this same code at an
    /// in-process server. A test that built its own link would be testing a
    /// second implementation of "how does this command reach a coordinator".
    pub fn at(origin: &str, token: Option<&str>) -> Result<Self, CommandFailure> {
        let origin = origin.trim_end_matches('/');
        if origin.is_empty() {
            return Err(CommandFailure::generic(
                "the coordinator URL is empty; a headless call needs a coordinator to answer",
            ));
        }
        if !origin.starts_with("http://") && !origin.starts_with("https://") {
            return Err(CommandFailure::generic(format!(
                "the coordinator URL {origin} is not an http or https origin"
            )));
        }
        if origin.starts_with("https://") {
            return Err(CommandFailure::generic(format!(
                "{origin} is https, and this build's Connect transport is plaintext-only. Point \
                 {COORD_URL_ENV} at the coordinator's own listener, or build roost-cli with \
                 connectrpc's `client-tls` feature."
            )));
        }
        let uri: Uri = origin.parse().map_err(|_| {
            CommandFailure::generic(format!("the coordinator URL {origin} cannot be parsed"))
        })?;
        let config = ClientConfig::new(uri).with_default_timeout(CALL_TIMEOUT);
        let config = match token {
            Some(token) => config.with_default_header("authorization", format!("Bearer {token}")),
            None => config,
        };
        Ok(Self {
            origin: origin.to_string(),
            client: CoordinatorServiceClient::new(HttpClient::plaintext(), config),
        })
    }

    /// The coordinator this link addresses, for a message.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The generated client. Every call in this module goes through it, which
    /// is what keeps a method name owned by the generator.
    #[must_use]
    pub fn stub(&self) -> &CoordinatorServiceClient<HttpClient> {
        &self.client
    }

    /// Await one generated call and read its answer, or say why there is none.
    pub async fn answer<V>(
        &self,
        call: impl Future<Output = Result<UnaryResponse<OwnedView<V>>, ConnectError>>,
    ) -> Result<V::Owned, CommandFailure>
    where
        V: MessageView<'static>,
    {
        self.try_answer(call).await.map_err(|(failure, _)| failure)
    }

    /// The same call, keeping the coordinator's own code beside the refusal.
    ///
    /// One code is worth a second shape: `FailedPrecondition` on a
    /// version-fenced write is the coordinator saying "something changed since
    /// you read", and it is the only refusal a caller may answer by reading
    /// again. Every other refusal is the coordinator's final word, and
    /// retrying it would re-run a command that already said no.
    pub async fn try_answer<V>(
        &self,
        call: impl Future<Output = Result<UnaryResponse<OwnedView<V>>, ConnectError>>,
    ) -> Result<V::Owned, (CommandFailure, ErrorCode)>
    where
        V: MessageView<'static>,
    {
        let response = call.await.map_err(|error| (self.refusal(&error), error.code))?;
        Ok(response.into_view().to_owned_message())
    }
    /// What a coordinator that did not answer, or refused, becomes.
    fn refusal(&self, error: &ConnectError) -> CommandFailure {
        let detail = error
            .message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("no detail");
        match error.code {
            ErrorCode::Unavailable => CommandFailure::generic(format!(
                "the coordinator at {} did not answer: {detail}",
                self.origin
            )),
            ErrorCode::Unimplemented => CommandFailure::generic(format!(
                "the coordinator at {} does not implement this call; it is older than this build",
                self.origin
            )),
            code => CommandFailure::generic(format!(
                "the coordinator at {} refused the call ({code:?}): {detail}",
                self.origin
            )),
        }
    }
}
