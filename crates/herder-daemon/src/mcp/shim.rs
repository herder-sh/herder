//! `herder mcp`: the stdio MCP server a vendor CLI spawns, piping it to the daemon.

use std::path::Path;

use anyhow::{Context, Result, bail};
use herder_protocol::SessionId;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{Hello, SOCKET, Welcome, token_path};

/// Runs the shim for `session` of the daemon on `data_dir` on this process's stdin and stdout,
/// until either side closes.
pub fn run_shim(data_dir: &Path, session: SessionId) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?
        .block_on(shim(
            data_dir,
            &session,
            tokio::io::stdin(),
            tokio::io::stdout(),
        ))
}

/// Authenticates as `session` with its token, then pipes `input` to the daemon and the daemon
/// to `output`. Returns when the daemon closes the connection, which it does once `input`
/// ends and its answers are sent.
pub async fn shim(
    data_dir: &Path,
    session: &SessionId,
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<()> {
    let path = token_path(data_dir, session)?;
    let token = tokio::fs::read_to_string(&path).await.with_context(|| {
        format!(
            "reading {}; herder starts this server itself, for a session it runs",
            path.display()
        )
    })?;
    let socket = data_dir.join(SOCKET);
    let stream = UnixStream::connect(&socket).await.with_context(|| {
        format!(
            "cannot reach the herder daemon at {}; is it running?",
            socket.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    let hello = Hello {
        session_id: session.clone(),
        token: token.trim().to_owned(),
    };
    let mut line = serde_json::to_string(&hello)?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    let mut read = BufReader::new(read);
    let mut line = String::new();
    read.read_line(&mut line)
        .await
        .context("reading the daemon's answer")?;
    match serde_json::from_str(&line).context("the daemon sent an invalid answer")? {
        Welcome::Ok => {}
        Welcome::Refused { message } => bail!("the herder daemon refused: {message}"),
    }
    let up = async {
        tokio::io::copy(&mut input, &mut write).await?;
        write.shutdown().await?;
        // The daemon's answers still to come decide when the shim ends.
        std::future::pending::<std::io::Result<()>>().await
    };
    let down = async {
        tokio::io::copy_buf(&mut read, &mut output).await?;
        output.flush().await
    };
    tokio::select! {
        result = up => result,
        result = down => result,
    }
    .context("relaying MCP messages")
}
