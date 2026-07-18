/*
 * Shared chrome logic for the agentflare dashboard (Alpine.js, no build
 * step). Every view loads this before /vendor/alpine.js and reuses:
 *   - afGetJson / afParams / afLink   — small fetch + query-string helpers
 *   - afResolveScope                  — default workspace/project pick
 *   - Alpine.data('shellNav', ...)    — the sidebar's workspace/project
 *                                        selectors and nav links
 *
 * The sidebar markup itself is copy-pasted into each page (see shell.html
 * for the reference copy) — there's no template include mechanism without
 * a build step, so the *behavior* lives here and only the HTML structure
 * is duplicated.
 */

/** GET `url` as JSON; returns `fallback` on any network/parse/HTTP error. */
async function afGetJson(url, fallback) {
  try {
    const res = await fetch(url);
    if (!res.ok) return fallback;
    return await res.json();
  } catch (_e) {
    return fallback;
  }
}

function afParams() {
  return new URLSearchParams(window.location.search);
}

/** Build an href for `path`, merging current query params with `overrides`. */
function afLink(path, overrides) {
  const params = afParams();
  for (const [k, v] of Object.entries(overrides || {})) {
    if (v === null || v === undefined || v === '') params.delete(k);
    else params.set(k, v);
  }
  const qs = params.toString();
  return qs ? `${path}?${qs}` : path;
}

/**
 * Resolve a workspace/project scope for a page load: use explicit
 * `?workspace_id=`/`?project_id=` if present, otherwise fall back to the
 * first workspace and first project in it. Every view does this
 * independently (rather than reading Alpine state from the sidebar
 * component) since navigation here is a plain full-page GET, not an SPA.
 */
async function afResolveScope(explicitWorkspaceId, explicitProjectId) {
  let workspaceId = explicitWorkspaceId || '';
  let projectId = explicitProjectId || '';
  if (!workspaceId) {
    const workspaces = await afGetJson('/api/pm/workspaces', []);
    if (workspaces.length) workspaceId = workspaces[0].id;
  }
  if (workspaceId && !projectId) {
    const projects = await afGetJson(
      `/api/pm/projects?workspace_id=${encodeURIComponent(workspaceId)}`,
      [],
    );
    if (projects.length) projectId = projects[0].id;
  }
  return { workspaceId, projectId };
}

function afFormatTime(unixSeconds) {
  if (!unixSeconds && unixSeconds !== 0) return '';
  return new Date(unixSeconds * 1000).toLocaleString();
}

document.addEventListener('alpine:init', () => {
  Alpine.data('shellNav', () => ({
    workspaces: [],
    projects: [],
    workspaceId: afParams().get('workspace_id') || '',
    projectId: afParams().get('project_id') || '',
    loading: true,

    async init() {
      this.workspaces = await afGetJson('/api/pm/workspaces', []);
      if (!this.workspaceId && this.workspaces.length) {
        this.workspaceId = this.workspaces[0].id;
      }
      if (this.workspaceId) {
        await this.loadProjects();
      }
      this.loading = false;
    },

    async loadProjects() {
      this.projects = await afGetJson(
        `/api/pm/projects?workspace_id=${encodeURIComponent(this.workspaceId)}`,
        [],
      );
      if (!this.projectId && this.projects.length) {
        this.projectId = this.projects[0].id;
      }
    },

    navHref(path) {
      return afLink(path, { workspace_id: this.workspaceId, project_id: this.projectId });
    },

    isActive(path) {
      return window.location.pathname === path;
    },

    onWorkspaceChange() {
      window.location.href = afLink(window.location.pathname, {
        workspace_id: this.workspaceId,
        project_id: null,
      });
    },

    onProjectChange() {
      window.location.href = afLink(window.location.pathname, {
        workspace_id: this.workspaceId,
        project_id: this.projectId,
      });
    },
  }));
});
