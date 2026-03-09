# Research Findings: Building AI Coding Agents for the Terminal

**Paper:** "Building AI Coding Agents for the Terminal: Scaffolding, Harness, Context Engineering, and Lessons Learned"
**Author:** Nghi D. Q. Bui (OpenDev)
**ArXiv:** 2603.05344v1 (March 5, 2026)
**Source:** https://arxiv.org/html/2603.05344v1
**License:** CC BY 4.0

> Note: The HTML could not be directly downloaded due to network proxy restrictions
> on the arxiv.org domain. Findings below were extracted via web search results,
> the ResearchGate listing, and the OpenDev GitHub repository.

## Paper Summary

The paper presents OpenDev, the first comprehensive technical report for an
open-source, terminal-native, interactive coding agent. It documents the shift
from IDE plugins to terminal-native agents that operate where developers manage
source control, execute builds, and deploy environments.

The architecture is organized around two phases:
- **Scaffolding**: assembles the agent (system prompt, tool schemas, subagent registry) before the first prompt
- **Harness**: orchestrates tool dispatch, context management, and safety enforcement at runtime

## Extracted Findings

### Finding 1: Compound AI System Architecture

Effective agents are not single monolithic LLMs but structured ensembles of
agents and workflows, each independently bound to a user-configured LLM. This
enables fine-grained optimization of cost, latency, and capability per workflow.

**Relevance to agcli:** agcli already supports this pattern via `ExecutionContext`
for shared state across commands. No changes needed, but documentation could
emphasize that agcli CLIs are designed to be composed into larger agent systems.

### Finding 2: Dual-Agent Architecture (Planning vs Execution)

Separating planning from execution at the schema level - the Planner subagent
receives a tool schema containing only read-only tools. Write tools simply do
not exist in its schema, making unsafe operations impossible by construction
rather than through runtime permission checks.

Three advantages:
1. No state machine means no risk of getting stuck in plan mode
2. The Planner can be spawned concurrently with other subagents
3. The tool surface area is reduced

**Relevance to agcli:** agcli commands could benefit from a `read_only` flag or
a way to tag commands as read-only vs mutating. This would allow agents to
construct separate tool schemas for planning vs execution phases.

### Finding 3: Adaptive Context Compaction

The system progressively reduces older observations to manage finite context
windows. For outputs exceeding 8,000 characters (~2,000 tokens):

1. Full output is written to a session-specific scratch directory
2. Replaced with a **500-character preview** plus a reference path
3. Agent sees enough to understand content and can read full output on demand

This creates a natural tiering system: preview in context, full data on disk.

**Relevance to agcli:** agcli's `truncate_lines_with_file()` already implements
this pattern but uses a line-count threshold. The paper suggests a
**character/token-based threshold** (~8,000 chars / ~2,000 tokens) and a
**preview snippet** (500 chars) may be more appropriate for agent consumption.
Consider adding character-based truncation alongside line-based truncation.

### Finding 4: Five Independent Safety Layers (Defense-in-Depth)

Because agents can execute arbitrary shell commands, overwrite files, and spawn
persistent processes, a single safety mechanism is insufficient. OpenDev employs
five independent safety layers, each preventing a class of harm independently.

The agent loop terminates through one of four paths:
1. Implicit completion (text response with no tool calls)
2. Explicit done signal via a completion tool
3. Error-recovery budget exhaustion (up to 3 targeted recovery messages per error sequence)
4. Reaching the safety iteration limit

**Relevance to agcli:** agcli's `retryable` field on errors aligns with path 3
(error-recovery budget). Consider adding:
- A `max_retries` field to error envelopes suggesting how many retries are appropriate
- A `severity` field (info/warn/error/fatal) to help agents decide whether to continue

### Finding 5: Event-Driven System Reminders to Counter Instruction Fade-Out

As conversations grow, critical instructions can be lost through context
compaction or simply pushed out of the attention window. OpenDev counteracts
"instruction fade-out" through event-driven system reminders - periodic
re-injection of critical behavioral guidance into the context.

**Relevance to agcli:** CLI tools should include critical operational context
in every response, not just the first one. The `next_actions` pattern already
does this for command discovery. Consider whether error envelopes should
re-state relevant system constraints (e.g., rate limits, required preconditions).

### Finding 6: Lazy Tool Discovery

Tools are loaded on demand rather than all at once, reducing the initial token
budget consumed by tool schemas. This is especially important when the agent
has access to many tools.

**Relevance to agcli:** The self-documenting command tree (root command) already
provides lazy discovery - agents call root to see what's available. But for CLIs
with many commands, consider supporting a `--brief` flag on root that returns
only command names and descriptions without full usage/params details.

### Finding 7: Automated Memory System

Cross-session knowledge accumulation where project-specific learnings persist
across sessions. This prevents the agent from re-discovering the same
information in every interaction.

**Relevance to agcli:** Not directly applicable to the CLI framework itself,
but CLIs built with agcli could include a `context` or `memory` field in their
envelopes containing persistent state hints for the calling agent.

### Finding 8: Three Overarching Design Principles

1. **Separation of concerns**: Each architectural decision (model selection,
   context management, safety enforcement, tool dispatch) should be independently
   configurable and replaceable.

2. **Progressive degradation**: The system should function gracefully as
   resources are exhausted (token budget, iteration count, network connectivity).

3. **Transparency over magic**: Every system action (tool calls, safety vetoes,
   context compaction, memory updates) should be observable and overridable.

**Relevance to agcli:** Principle 3 (transparency) reinforces agcli's JSON-always
approach. Principle 2 (progressive degradation) suggests that truncation and
error handling should gracefully degrade rather than hard-fail. The `retryable`
field already supports this for errors.

### Finding 9: Schema-Level Access Control

Rather than runtime permission checks, the Planner's tool schema simply omits
write tools. This is enforced at the schema level - the tools don't exist in
the Planner's view of the world.

**Relevance to agcli:** Commands could be tagged with a `mutates: bool` field
in the self-documenting tree output. This would allow agents to filter commands
based on their current mode (planning vs execution).

### Finding 10: Four-Layer Persistence Architecture

State is persisted across four stores:
1. **Config Manager**: project-local → user-global → env-var → built-in defaults
2. **Session Manager**: full conversation histories as JSON
3. **Provider Cache**: model capability metadata cached locally
4. **Operation Log**: file changes tracked for rollback

**Relevance to agcli:** The operation log pattern (tracking changes for rollback)
could be exposed in CLI responses. When a command mutates state, include a
`rollback` next_action template that undoes the change.

## Recommendations for agcli

Based on these findings, the following changes are recommended:

### Changes to Existing Features

1. **Add character-based truncation** alongside `truncate_lines_with_file()`:
   - New function: `truncate_chars_with_file(content, max_chars, preview_chars, file_prefix)`
   - Default thresholds: 8,000 chars max, 500 chars preview (per paper's findings)
   - Returns a `TruncatedContent` with preview + file path

2. **Add `max_retries` to error envelopes**: Optional field suggesting retry budget
   - Complements `retryable: bool` with a concrete number
   - Helps agents implement error-recovery budget exhaustion (Finding 4)

3. **Add `mutates` field to command documentation**: Tag commands as read-only or mutating
   - Appears in `CommandDoc` and root command tree output
   - Enables agents to construct read-only tool schemas for planning (Finding 2)

### New Recommendations for design.md

4. **Add "Progressive Degradation" anti-pattern**: CLIs should degrade gracefully
   when resources are exhausted, not hard-fail

5. **Add "Rollback Actions" pattern**: Mutating commands should include undo
   next_actions when reversible

6. **Add "Context Budget Awareness" principle**: CLI output should be designed
   with token budgets in mind (~2,000 token threshold for large outputs)

### Documentation Updates

7. **Add reference to this paper** in design.md as supporting research
8. **Add "Lazy Discovery" pattern** documenting the `--brief` root option for
   large command trees
