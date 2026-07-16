// agentflare.dev — Cloudflare Worker.
//
// Serves the static landing page from the ASSETS binding (./public), with one
// exception: the install endpoints proxy the real installer straight from the
// repo so `curl -fsSL https://agentflare.dev/install.sh | sh` works, and the
// hero command on the page resolves against the domain instead of a raw
// githubusercontent URL.

type Fetcher = { fetch(request: Request): Promise<Response> };

interface Env {
  ASSETS: Fetcher;
}

// Pin the installer to an immutable release tag so a change on `master` can't
// silently alter what users execute via `curl … | sh`. Bump on release when
// install.sh / install.ps1 change.
const INSTALL_REF = "v1.3.1";
const RAW = `https://raw.githubusercontent.com/getappz/agentflare/${INSTALL_REF}`;

// path on agentflare.dev -> file in the repo
const INSTALL_SCRIPTS: Record<string, string> = {
  "/install.sh": `${RAW}/install.sh`,
  "/install.ps1": `${RAW}/install.ps1`,
};

function installerUnavailable(): Response {
  return new Response(
    "# agentflare installer is temporarily unavailable — try:\n" +
      "#   cargo install --git https://github.com/getappz/agentflare\n",
    { status: 502, headers: { "content-type": "text/plain; charset=utf-8" } },
  );
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    const source = INSTALL_SCRIPTS[url.pathname];
    if (source) {
      let upstream: Response;
      try {
        upstream = await fetch(source, {
          // edge-cache the installer for 5 min; it changes rarely
          cf: { cacheTtl: 300, cacheEverything: true },
        });
      } catch {
        // DNS / TLS / connection failure — fetch rejects before returning a Response
        return installerUnavailable();
      }
      if (!upstream.ok) {
        return installerUnavailable();
      }
      return new Response(upstream.body, {
        status: 200,
        headers: {
          "content-type": "text/plain; charset=utf-8",
          "cache-control": "public, max-age=300",
        },
      });
    }

    return env.ASSETS.fetch(request);
  },
};
