import { describe, expect, it } from "vitest";
import { discoverRepos, discoveryOf, discoveryQuery, type Discovery } from "../src/repo-discovery";
import type { RepoView } from "../src/hub";

const empty: Discovery = { q: "", owner: "", visibility: "all", sort: "recent", page: 1 };
const repo = (name: string, owner: string, isPublic: boolean, at: number): RepoView => ({ repo: name, org: owner, public: isPublic, created_ms: at, trunk: `${owner}-${name}`, description: "Fast edge web framework", upstream: null, summary: null });
const repos = [repo("web", "studio", true, 20), repo("receipts", "studio", false, 30), repo("web", "tools", true, 10)];
describe("repository discovery", () => {
  it("combines words, owner, and visibility filters over authorized input", () => {
    expect(discoverRepos(repos, { ...empty, q: "studio edge", visibility: "public" }).repos.map((row) => row.repo)).toEqual(["web"]);
    expect(discoverRepos(repos, { ...empty, owner: "studio", visibility: "private" }).repos.map((row) => row.repo)).toEqual(["receipts"]);
    expect(discoverRepos(repos, { ...empty, owner: "missing" }).total).toBe(0);
    expect(discoverRepos(repos.filter((row) => row.public), { ...empty, visibility: "private" }).total).toBe(0);
  });
  it("paginates, clamps old page numbers, and does not mutate the original collection", () => {
    const many = Array.from({ length: 45 }, (_, index) => repo(`repo-${index}`, "studio", true, index));
    expect(discoverRepos(many, { ...empty, page: 2 }).repos).toHaveLength(20);
    expect(discoverRepos(many, { ...empty, page: 99 }).repos).toHaveLength(5);
    expect(discoverRepos(many, { ...empty, q: "repo-44", page: 99 }).page).toBe(1);
    expect(many[0].repo).toBe("repo-0");
  });
  it("round trips shared search URLs and gives invalid parameters safe defaults", () => {
    const state = { ...empty, q: "receipt formatter", owner: "studio", visibility: "private" as const, sort: "name" as const, page: 2 };
    expect(discoveryOf(new URLSearchParams(discoveryQuery(state)))).toEqual(state);
    expect(discoveryOf(new URLSearchParams("page=-3&visibility=invalid&sort=invalid"))).toEqual(empty);
  });
});
