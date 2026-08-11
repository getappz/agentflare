---
name: openapi-documentation
description: Use when writing or maintaining an OpenAPI/Swagger 3.0 specification for a REST API — new endpoints, request/response schemas, auth schemes, or error responses that need to be documented accurately and stay in sync with the implementation. Skip for internal-only functions/modules with no HTTP surface, and skip for GraphQL APIs — this is REST/OpenAPI specific.
---

# OpenAPI Documentation

Write OpenAPI 3.0 specs that are accurate enough to generate a working
client from, not just descriptive prose that happens to be near the code.

## When to use

- Creating or updating an OpenAPI/Swagger spec for a REST API.
- An endpoint's request/response shape, auth requirement, or error surface
  changed and the spec needs to catch up.
- Skip for non-HTTP internal APIs/functions, and skip for GraphQL (which
  has its own schema/introspection story, not OpenAPI's).

## Key responsibilities

1. Create OpenAPI 3.0-compliant specifications — validate against the spec,
   not just "looks right."
2. Document every endpoint with both a summary and a fuller description —
   the summary is what shows up in a collapsed list view, so it has to
   stand alone.
3. Define request/response schemas accurately, including every field's
   type and whether it's required.
4. Include authentication and security schemes — an endpoint's auth
   requirement is part of its contract, not an implementation detail to
   omit.
5. Provide a real example for every operation — a schema without an
   example makes the reader reconstruct a valid payload by hand.

## Best practices

- Use descriptive summaries and descriptions — "Get user" tells a reader
  nothing an endpoint path didn't already say; "Get a user's profile,
  including their current subscription tier" does.
- Include example requests *and* responses, not just one or the other.
- Document every realistic error response (400/401/403/404/409/5xx), not
  just the 200 case — the error surface is as much a contract as success.
- Use `$ref` for reusable components (schemas, responses, parameters) —
  duplicated inline schemas drift out of sync with each other.
- Follow the OpenAPI 3.0 specification strictly, not "close enough" —
  strict compliance is what makes codegen and client tooling actually work.
- Group endpoints logically with tags so the generated docs UI is
  navigable, not one flat list.

## Structure

```yaml
openapi: 3.0.0
info:
  title: API Title
  version: 1.0.0
  description: API Description
servers:
  - url: https://api.example.com
paths:
  /endpoint:
    get:
      summary: Brief description
      description: Detailed description
      parameters: []
      responses:
        '200':
          description: Success response
          content:
            application/json:
              schema:
                type: object
              example:
                key: value
components:
  schemas:
    Model:
      type: object
      properties:
        id:
          type: string
```

## Documentation elements to include

- Clear, stable `operationId`s (client generators use these as method
  names — renaming one is a breaking change for generated clients).
- Request and response examples for every operation.
- Full error response documentation, with the actual error shape the API
  returns.
- Security requirements per operation (not just a global default that
  silently doesn't apply everywhere).
- Rate-limiting behavior, when the API has any — callers need to know the
  limits exist before they hit them in production.
