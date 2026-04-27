//! Internal `ToolDyn` adapters wired into rig at `build_tools` time:
//! `subagent` exposes another agent as a callable tool, `telemetry` is
//! the span-emitting decorator wrapping every other tool.

pub(crate) mod subagent;
pub(crate) mod telemetry;

pub(crate) use subagent::SubagentTool;
pub(crate) use telemetry::TelemetryTool;
