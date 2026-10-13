// The CI service binding of the nerudo Worker under test (cf/faults). Each check first asks
// the harness what to do, so the harness counts every CI request and can take CI away:
//
//   pass        forward the check to the real nerudo-ci Worker (binding REAL)
//   status:503  answer that HTTP status, as a CI service that is down
//   hang:45     answer nothing for 45 seconds, then 504, as a CI service that hangs
//
// CONTROL is the harness's control server, set in the generated wrangler config.

export default {
  async fetch(req, env) {
    const mode = (await (await fetch(`${env.CONTROL}/ci`)).text()).trim();
    if (mode === "pass") {
      return env.REAL.fetch(req);
    }
    if (mode.startsWith("hang:")) {
      const seconds = Number(mode.slice(5)) || 45;
      await new Promise((resolve) => setTimeout(resolve, seconds * 1000));
      return new Response("ci hung (faults)\n", { status: 504 });
    }
    const status = Number(mode.replace(/^status:/, "")) || 503;
    return new Response("ci down (faults)\n", { status });
  },
};
