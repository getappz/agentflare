---
name: algorithm-pseudocode-design
description: Use when an algorithm is non-trivial enough that getting the logic right matters more than the syntax — concurrency-sensitive code, a non-obvious data-structure choice, or anything where a subtle off-by-one or complexity mistake is expensive to find after implementation. Write the algorithm in language-agnostic pseudocode with an explicit complexity analysis before writing real code. Skip for straightforward CRUD or glue code where the implementation is a direct restatement of the requirement — pseudocode would just be the code with different syntax.
---

# Algorithm & Pseudocode Design

Design the algorithm before committing to a language's syntax. Pseudocode
forces you to separate "what is the logic" from "how does this language
express it" — bugs found at this stage are far cheaper than bugs found
after a full implementation.

## When to use

- The algorithm is non-trivial: non-obvious data structure choice, tricky
  control flow, concurrency, or a correctness property that's easy to get
  subtly wrong (rate limiting, cache eviction, ranking/scoring).
- Complexity matters and hasn't been thought through yet — "this might be
  O(n²) on the hot path" is worth catching on paper.
- Skip for code that's a direct, obvious translation of the requirement
  (simple CRUD, straightforward glue/plumbing) — pseudocode adds no value
  there.

## Structure and syntax

Keep it language-agnostic — no language-specific syntax, so the logic
reads the same regardless of what it'll be implemented in:

```
ALGORITHM: AuthenticateUser
INPUT: email (string), password (string)
OUTPUT: user (User object) or error

BEGIN
    IF email is empty OR password is empty THEN
        RETURN error("Invalid credentials")
    END IF

    user ← Database.findUserByEmail(email)
    IF user is null THEN
        RETURN error("User not found")
    END IF

    isValid ← PasswordHasher.verify(password, user.passwordHash)
    IF NOT isValid THEN
        SecurityLog.logFailedLogin(email)
        RETURN error("Invalid credentials")
    END IF

    session ← CreateUserSession(user)
    RETURN {user: user, session: session}
END
```

## Data structure selection

Name the structure, its complexity per operation, and *why* it was chosen
over the alternatives — not just what it is:

```
UserCache:
    Type: LRU cache with TTL
    Purpose: reduce DB queries for active users
    Operations: get O(1), set O(1), evict O(1)

PermissionTree:
    Type: trie (prefix tree)
    Purpose: efficient hierarchical permission checks
    Operations: hasPermission(path) O(m), m = path length
```

## Algorithm patterns worth writing out explicitly

Patterns where the naive implementation is subtly wrong are worth a full
pseudocode pass — e.g. a token-bucket rate limiter (refill math has to
account for elapsed time correctly, not just decrement a counter), or a
scored-ranking search (weighting, recency boost, tie-breaking all need to
be decided before code, not discovered by trial and error in review).

## Complexity analysis

For every algorithm designed this way, state time and space complexity
per step, then the total — and call out the dominant term:

```
Time Complexity:
    Query preprocessing: O(m)      m = query length
    Index lookup:         O(k log n)  k = token count
    Scoring:               O(p)      p = candidate count
    Sorting:                O(p log p)
    Total: O(p log p), dominated by sorting

Optimization notes:
    - Use an inverted index for O(1) token lookup
    - Consider approximate ranking above ~10k candidates
```

Skipping this step is how an algorithm that's fine at test scale becomes
a production incident at real scale.

## Design patterns in pseudocode

When a pattern (Strategy, Observer, etc.) clarifies the structure, sketch
the interface and key methods in pseudocode before implementation — it
surfaces interface mismatches early, when they're a five-minute fix
instead of a refactor.

## Best practices

1. **Language-agnostic** — no language-specific syntax; the logic should
   read the same in any implementation language.
2. **Clear logic over cleverness** — focus on algorithm flow, not
   implementation tricks.
3. **Handle edge cases in the pseudocode itself** — null inputs, empty
   collections, concurrent access — not as an afterthought during coding.
4. **Always state complexity** — time and space, for every non-trivial
   algorithm.
5. **Meaningful names** — variable names should explain purpose even
   without types.
6. **Break complex algorithms into subroutines** — a 100-line BEGIN/END
   block is as hard to review as a 100-line function.

## Handoff

Once the algorithm and its complexity are pinned down, implement it
following `superpowers:test-driven-development` — write the failing test
from the pseudocode's stated behavior first, then implement.
