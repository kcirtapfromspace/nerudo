import { forward } from "../../../lib/nerudo";

// The sequencer's own routes, through this site: the API, the agents' endpoints and the
// classic pages all stay reachable on one origin.
export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const DELETE = forward;
