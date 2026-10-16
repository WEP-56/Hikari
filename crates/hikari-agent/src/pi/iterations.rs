// Ported from pi_agent_rust b6fa9e18b, src/agent.rs. See licenses/pi_agent_rust.LICENSE.
use tracing::warn;
pub const MAX_TOOL_ITERATIONS_DEFAULT: usize = 50;

/// Sanity ceiling for `max_tool_iterations` overrides.
///
/// Guards against runaway loops from a typo while still leaving plenty of
/// room for long, multi-step agentic tasks (large refactors, multi-phase
/// spec implementations).
pub const MAX_TOOL_ITERATIONS_CEILING: usize = 1_000;

/// Maximum automatic continuations after an Anthropic `pause_turn` stop.
///
/// Anthropic documents `pause_turn` as a successful, resumable response from
/// long-running server tools. Retrying indefinitely would let a broken remote
/// tool turn one user request into an unbounded loop, so keep the provider's
/// recommended retry budget explicit and small.
pub const MAX_PAUSE_TURN_CONTINUATIONS: usize = 3;


/// Resolve the effective tool-iteration cap from `PI_MAX_TOOL_ITERATIONS`.
///
/// Falls back to [`MAX_TOOL_ITERATIONS_DEFAULT`] when unset/invalid. Used
/// by callers that build an [`AgentConfig`] without going through the CLI
/// parser (ACP server, SDK).
pub fn resolved_max_tool_iterations_default() -> usize {
    resolve_max_tool_iterations(std::env::var("PI_MAX_TOOL_ITERATIONS").ok().as_deref())
}

/// Pure resolver for `max_tool_iterations` string overrides.
///
/// Returns [`MAX_TOOL_ITERATIONS_DEFAULT`] when input is `None`, empty,
/// unparseable, zero, or above the ceiling — emitting a warning so a
/// misconfigured cap is observable in logs rather than silently lost.
pub fn resolve_max_tool_iterations(raw_override: Option<&str>) -> usize {
    let Some(raw) = raw_override.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return MAX_TOOL_ITERATIONS_DEFAULT;
    };
    match raw.parse::<usize>() {
        Ok(0) => {
            warn!(
                "PI_MAX_TOOL_ITERATIONS=0 is invalid; falling back to {}",
                MAX_TOOL_ITERATIONS_DEFAULT
            );
            MAX_TOOL_ITERATIONS_DEFAULT
        }
        Ok(n) if n > MAX_TOOL_ITERATIONS_CEILING => {
            warn!(
                "PI_MAX_TOOL_ITERATIONS={n} exceeds ceiling {MAX_TOOL_ITERATIONS_CEILING}; clamping to {MAX_TOOL_ITERATIONS_CEILING}"
            );
            MAX_TOOL_ITERATIONS_CEILING
        }
        Ok(n) => n,
        Err(err) => {
            warn!(
                "PI_MAX_TOOL_ITERATIONS={raw:?} is not a valid usize ({err}); falling back to {}",
                MAX_TOOL_ITERATIONS_DEFAULT
            );
            MAX_TOOL_ITERATIONS_DEFAULT
        }
    }
}

/// Clamp a CLI-parsed `Option<usize>` cap to the supported range.
///
/// Same semantics as [`resolve_max_tool_iterations`] but for values that
/// have already been parsed by clap. Returns the effective cap, clamped
/// to `[1, MAX_TOOL_ITERATIONS_CEILING]` with invalid values (None, 0)
/// falling back to [`MAX_TOOL_ITERATIONS_DEFAULT`].
pub fn clamp_max_tool_iterations(value: Option<usize>) -> usize {
    match value {
        None => MAX_TOOL_ITERATIONS_DEFAULT,
        Some(0) => {
            warn!(
                "--max-tool-iterations=0 is invalid; falling back to {}",
                MAX_TOOL_ITERATIONS_DEFAULT
            );
            MAX_TOOL_ITERATIONS_DEFAULT
        }
        Some(n) if n > MAX_TOOL_ITERATIONS_CEILING => {
            warn!(
                "--max-tool-iterations={n} exceeds ceiling {MAX_TOOL_ITERATIONS_CEILING}; clamping to {MAX_TOOL_ITERATIONS_CEILING}"
            );
            MAX_TOOL_ITERATIONS_CEILING
        }
        Some(n) => n,
    }
}
