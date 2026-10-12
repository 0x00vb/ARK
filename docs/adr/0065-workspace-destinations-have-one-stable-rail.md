# 0065. Workspace destinations have one stable rail

Status: Accepted
Date: 2026-10-11

## Context

Chat, Agents and Board are views of the current project. Schedules apply across that project and
were only discoverable from the chat that made them. Closing the project sidebar also removed the
only stable way to move around the workspace. Duplicating every view in another sidebar would add
controls without clarifying the hierarchy.

## Decision

ARK has one labelled destination rail. Home restores the last available Chat, Agents or Board
view. Schedules opens one project-wide list. The rail remains when the project sidebar closes and
becomes a bottom bar in a compact window. Settings has no destination selected.

A project change clears schedule rows before loading the next project. Late replies cannot cross
that boundary. A failed refresh keeps same-project rows with a retry action; a first failure,
loading and empty state are distinct. Keyboard navigation moves focus to the destination heading.

## Consequences

The top bar remains the direct selector inside Home, so ARK does not duplicate Chat, Agents or
Board. The rail costs 64 pixels in a wide window and no workspace height; compact windows reserve
one safe-area-aware bottom row. Later project-wide destinations must earn a place in this rail and
must not repeat a Home subview or side-pane tool.

## Check

The WebKit page tests cover Home restoration, keyboard focus, wide and compact geometry, every
schedule state, offline recovery and project-switch races. Desktop material tests cover the rail
in translucent, solid and reduced-transparency appearances.
