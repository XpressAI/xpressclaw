# ADR 000: Version Control Workflow

**Status:** Approved

**Context:**

A disciplined version control workflow is critical for maintaining a clean, understandable, and stable codebase. It enables effective collaboration, simplifies debugging, and preserves the history of not just *what* changed, but *why* it changed.

**Decision:**

We will adopt a Git-based workflow with a strong emphasis on clear, logical commits and detailed contextual notes.

1.  **Version Control System:** We will use **Git** for all source code management.
2.  **ADR-Driven Development:** All new features or significant changes must be preceded by an approved ADR. Commits implementing a feature must reference the corresponding ADR.
3.  **Logical Commits:** Commits will be small, atomic, and represent a single logical change. The application must be in a working, stable state at each commit.
4.  **Commit Messages:** Commit messages will follow the [Conventional Commits](https://www.conventionalcommits.org/) specification to create an explicit and human-readable commit history.
5.  **Contextual Notes (`git notes`):** To capture the rich context and decision-making process behind the code, we will use `git notes`.
    *   After each commit, a note will be added to the `refs/notes/agent` namespace.
    *   This note will contain the "thinking" process, relevant snippets of our conversation, and any other context that informed the changes in that commit.
    *   This practice will serve as a "developer diary," providing invaluable insights for future development and debugging.

**Consequences:**

*   **Pros:**
    *   **Deep Context:** `git notes` provides a powerful, co-located mechanism for storing rich context without cluttering commit messages.
    *   **Traceability:** The link between ADRs, commits, and notes creates a clear, end-to-end trail for every change.
    *   **Improved Maintainability:** Future developers (including our future selves) will have a much easier time understanding the rationale behind the code.
*   **Cons:**
    *   **Discipline Required:** This workflow requires a high degree of discipline to maintain consistently.
    *   **`git notes` Obscurity:** `git notes` is a less common feature of Git, so new contributors may need a brief introduction to the concept. This is a small price to pay for the benefits.
