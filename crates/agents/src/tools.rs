//! Internal `ToolDyn` adapters wired into rig at `build_tools` time:
//! `dispatch_task` enqueues fire-and-forget background work, `subagent`
//! exposes another agent as an inline callable tool, `tasks_status` reads
//! the queue so an orchestrator can answer "what's going on?" from chat,
//! `telemetry` is the span-emitting decorator wrapping every other tool.

pub(crate) mod dispatch_task;
pub(crate) mod subagent;
pub(crate) mod tasks_status;
pub(crate) mod telemetry;

pub(crate) use dispatch_task::DispatchTaskTool;
pub(crate) use subagent::SubagentTool;
pub(crate) use tasks_status::TasksStatusTool;
pub(crate) use telemetry::TelemetryTool;
