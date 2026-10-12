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

/// Threshold (as a fraction of `max_tool_iterations`) at which the runtime
/// emits a one-shot soft-handoff steering message so the agent can begin a
/// graceful incomplete-handoff rather than being silently killed at the cap.
/// Encoded as numerator/denominator to avoid floating-point in a hot loop.
const ITERATION_WARN_NUMERATOR: usize = 4;
const ITERATION_WARN_DENOMINATOR: usize = 5;

/// Below this absolute cap, the soft-handoff warning is suppressed — for
/// caps like 3 or 4, the warning would fire on the first iteration and add
/// noise rather than help.
const ITERATION_WARN_MIN_CAP: usize = 5;

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

/// Pure predicate: should we emit the one-shot iteration-budget warning at
/// the current iteration, given the configured cap?
///
/// Fires when `current >= (max * 4) / 5` and `max >= ITERATION_WARN_MIN_CAP`.
/// Caller is responsible for tracking fire-once state so the steering message
/// only injects once per run-loop. Stateless and integer-only so it's safe to
/// call inside the hot loop. Uses `saturating_mul` so an SDK caller that
/// writes `AgentConfig::max_tool_iterations = usize::MAX` directly (bypassing
/// the resolvers' clamp) gets a sane "never warn" rather than wrap-around to
/// a tiny threshold.
pub const fn should_warn_at_iteration_threshold(current: usize, max: usize) -> bool {
    max >= ITERATION_WARN_MIN_CAP
        && current >= max.saturating_mul(ITERATION_WARN_NUMERATOR) / ITERATION_WARN_DENOMINATOR
}

/// Body of the one-shot soft-handoff steering message, formatted with the
/// current/max iteration counts. Kept as a free function so test fixtures
/// can pin the wording without instantiating a full agent.
pub fn iteration_handoff_steering_text(current: usize, max: usize) -> String {
    format!(
        "[runtime] Tool-iteration budget at >=80% (used {current} of {max}). \
         Per the iteration-aware-handoff protocol in your spec, begin graceful \
         handoff now: commit current work, post a one-line status note, and \
         write an incomplete-handoff envelope with what's done / what remains \
         / next-agent starting position. Do NOT compress remaining work into \
         the last few iterations."
    )
}
