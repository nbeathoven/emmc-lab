# TUI Requirements Template

This document is intended to be reused as a requirements template for other terminal applications, especially operator tools, diagnostics consoles, and safety-sensitive workflows.

## 1. Product Intent

The application shall present itself as an operator console, not as a decorative full-screen interface.

The TUI shall optimize for:

- SSH and remote-terminal use
- constrained terminal sizes
- keyboard-only operation
- explicit operational context
- safety before execution
- dense but readable information display

The TUI should feel predictable, technical, and low-friction. It should not depend on mouse input, animations, or wide-screen assumptions.

## 2. Design Principles

- `SSH-first`: the interface must remain usable over remote shells, unstable connections, and ordinary terminal emulators.
- `Information before decoration`: every screen must prioritize state, target, risk, and result clarity over visual flourish.
- `Progressive disclosure`: show the next decision clearly, then reveal deeper detail in a side panel or follow-up screen.
- `Keyboard-first`: every meaningful action must be available from the keyboard with obvious, repeated key hints.
- `Safe by default`: destructive or state-changing actions must be clearly labeled, reviewed, and confirmed.
- `Terminal realism`: layouts must adapt to narrow and short terminals by stacking panels, truncating carefully, and reducing visible rows.
- `Consistent mental model`: menus, forms, selectors, confirmations, reports, and results must behave the same way across workflows.
- `CLI parity`: the TUI should be a guided layer over the same underlying actions used by direct commands, not a separate product.
- `Plain language`: labels, warnings, and help text must describe scope, risk, and outcome directly.
- `Deterministic output`: tables, summaries, and controls should render consistently without hidden state or visual ambiguity.

## 3. Visual Language

The visual system shall be restrained and text-led.

- Use clear section titles, borders, dividers, and fixed table structure.
- Use color only as reinforcement for hierarchy and status, never as the only signal.
- Respect `NO_COLOR` and degrade cleanly to plain text.
- Prefer high-contrast emphasis for titles, warnings, and selected rows.
- Use short, technical labels such as `Scope`, `Risk`, `Context`, `Status`, `Details`, `Checks`, and `Capabilities`.
- Avoid visual noise such as excessive box nesting, icon-heavy controls, or screen-filling chroma.

## 4. Layout Requirements

Every major screen shall follow a stable frame.

- `Header`: application name, version, and current host or environment.
- `Breadcrumb/Path`: current workflow position.
- `Body`: primary working area, usually split into a selection or content pane plus a detail pane.
- `Footer`: active keyboard hints for the current screen.

Layout behavior shall follow these rules:

- Prefer a two-panel layout when width permits.
- Fall back to stacked sections when side-by-side presentation would reduce readability.
- Clamp expectations to practical terminal sizes rather than assuming large screens.
- Preserve the most important context and controls on short terminals, even if lower-priority detail is reduced.
- Use tables for structured comparisons and key-value panels for concise status summaries.

## 5. Interaction Model

The TUI shall use a small, repeatable key vocabulary.

- `Up/Down` and `j/k`: move selection or scroll.
- `Enter`: open, edit, confirm, or execute the currently focused item.
- `Esc`: go back, cancel the current edit, or leave the current workflow level.
- `q`: quit from non-edit contexts.
- `Ctrl-C`: immediate termination when supported.
- `Tab`: optional mode toggle when there is a single obvious secondary option.

The footer of every screen shall restate the currently valid keys.

The same key must not mean different things in equivalent contexts across screens.

## 6. Screen Types

The application should be built from a small set of screen archetypes.

- `Menu Screen`: shows actions in a table with a detail pane describing purpose, scope, and recommended use.
- `Selector Screen`: shows selectable records with a current-row detail summary.
- `Form Screen`: shows editable fields, defaults, required status, and one explicit execute row.
- `Confirmation Screen`: shows the exact target, planned changes, and risk before applying.
- `Result Screen`: shows execution output in the main pane and summarized status/context in a side pane.
- `Report Screen`: shows saved-session or historical output in structured summary form.
- `Live Monitor Screen`: shows updated metrics and top offenders while keeping controls obvious.
- `Settings/Doctor Screen`: shows environment facts, capability checks, and actionable warnings.

These screen types shall share layout, navigation, and language conventions.

## 7. Content Requirements

Each screen shall answer the following questions without forcing the user to infer them:

- Where am I?
- What object or target am I acting on?
- What can I do next?
- What is the operational risk?
- What happens if I press `Enter` here?

Tables and summaries shall prefer:

- explicit units
- truncated but still meaningful values
- right alignment for numeric columns
- left alignment for labels and paths
- stable column headers
- visible selection markers

Detail panes should explain the currently selected item, not generic application help.

## 8. Safety Requirements

State-changing or destructive actions shall require stronger framing than read-only actions.

- Every action must declare a visible `Scope` and `Risk`.
- Destructive or configuration-writing actions must route through a review screen.
- Confirmation screens must show the exact target and planned change set.
- The user must be able to back out with `Esc` before execution.
- Safety text must be specific about what changes, where, and with what consequences.
- Read-only and state-changing actions must be visually and linguistically distinguishable.

## 9. Responsiveness and Degradation

The TUI shall remain useful in imperfect terminal environments.

- Detect terminal dimensions and adapt layout accordingly.
- Use reduced row counts on short screens.
- Truncate text deliberately instead of allowing broken wrapping to destroy tables.
- Disable or reduce color when the terminal does not support it.
- If a true TTY is unavailable, fail clearly or fall back to plain CLI behavior.
- When leaving the managed screen for shell-based subprocesses, restore the terminal cleanly and resume cleanly.

## 10. Data Presentation Rules

- Show overview metadata first, then deeper diagnostics.
- Pair summaries with details whenever possible.
- Separate operational metrics from interpretation notes.
- Keep attribution caveats visible when data is partial, best-effort, or permission-limited.
- When a list is long, show the highest-value rows first and explain what is omitted.
- Prefer explicit timestamps, durations, rates, counts, and byte units.

For monitoring and reporting interfaces:

- distinguish between current state and cumulative totals
- keep control hints visible during live updates
- preserve operator trust by labeling fallback or incomplete data clearly

## 11. Form and Wizard Requirements

Forms shall behave like controlled operator input, not open-ended text documents.

- Each field must have a label, current value, default value, required/optional state, and field-specific description.
- The execute action must appear as a dedicated final row.
- Entering edit mode must be obvious.
- `Esc` inside edit mode must cancel only the edit, not the entire form.
- `Esc` outside edit mode must leave the form without executing.
- Required fields must be enforced before execution.
- Field descriptions should explain impact and expected format, not just repeat the label.

## 12. Implementation Template

Use the following structure when specifying a new application TUI:

### App Summary

- Application name: `[name]`
- Primary operator: `[user type]`
- Runtime environment: `[local terminal / SSH / serial / mixed]`
- Primary risk class: `[read-only / local-write / remote-write / destructive]`

### Required Screens

- Main menu with action table and detail pane
- One selector screen for choosing targets or saved sessions
- One form or wizard flow for guided execution
- One confirmation screen for state-changing actions
- One result/report screen for outcome review
- One settings or doctor screen for environment validation

### Global Interaction Rules

- Navigation keys: `[define exact keys]`
- Quit behavior: `[define where q is valid]`
- Back behavior: `[define Esc semantics]`
- Execute behavior: `[define Enter semantics]`
- Non-TTY behavior: `[define fallback]`

### Global Layout Rules

- Header content: `[app name, version, host, target context]`
- Breadcrumb format: `[workflow path format]`
- Body layout: `[two-pane by default, stacked fallback]`
- Footer content: `[screen-specific key hints]`

### Safety Rules

- Scope labels required: `yes/no`
- Risk labels required: `yes/no`
- Confirmation required for writes: `yes/no`
- Confirmation required for destructive actions: `yes/no`
- Review must show exact target: `yes/no`

### Data Display Rules

- Preferred table columns: `[list]`
- Numeric alignment: `right`
- Text alignment: `left`
- Truncation rule: `[ellipsis / hard cut / wrap by panel]`
- Empty value rule: `[use "-" or equivalent]`

### Terminal Constraints

- Minimum supported width: `[value]`
- Minimum supported height: `[value]`
- Color optional: `yes`
- `NO_COLOR` supported: `yes`
- Alternate screen usage: `[always / optional / never]`

## 13. Acceptance Criteria

An implementation should be considered aligned with this design when:

- a new user can move through the main workflow without guessing the controls
- every screen exposes current context and next action clearly
- risky operations are distinguishable before execution
- the interface remains readable on a small SSH terminal
- tables stay structurally readable under width pressure
- the app can suspend and resume terminal control without leaving the shell corrupted
- operators can recover from mistakes with `Esc` before execution
- result and report screens explain what happened, not just that something happened

## 14. Non-Goals

The template does not require:

- mouse support
- decorative animation
- pixel-perfect full-screen dashboards
- deep visual theming
- modal complexity beyond what improves safety or clarity

## 15. Short Principle Summary

If this design language has to be reduced to one sentence:

Build a terminal operator console that is explicit, safe, keyboard-first, dense with useful context, and honest about the limits of the terminal it runs in.
