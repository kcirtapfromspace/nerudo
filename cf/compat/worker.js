// Keep existing API/Git URLs and bookmarks attached to the same forge during migration.
// Service bindings preserve the request origin, body, headers, and streamed response.
export default {
  fetch(request, env) {
    return env.NERUDO.fetch(request);
  },
};
