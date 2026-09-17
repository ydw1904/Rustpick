import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"

// Exercise the inline UI predicate without loading React or the Tauri runtime.
const source = readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8")
const body = source.match(/const visibleCampaigns = inventory\.filter\(\(c\) => \{([\s\S]*?)\n  \}\)\.sort/)?.[1]
assert.ok(body, "inventory filter predicate exists")
const visible = new Function("c", "filters", "settings", "inv", body)

test("Not linked filters by account connection, including earnable badge campaigns", () => {
  const filters = { notlinked: false, upcoming: true, expired: true, excluded: false, finished: true }
  const campaign = { game: "Game", name: "Campaign", status: "Active", total: 1, claimed: 0 }
  for (const needs_link of [false, true]) {
    const unlinked = { ...campaign, linked: false, needs_link }
    assert.equal(visible(unlinked, filters, null, { search: "" }), false)
    assert.equal(visible(unlinked, { ...filters, notlinked: true }, null, { search: "" }), true)
  }
  assert.equal(visible({ ...campaign, linked: true, needs_link: false }, filters, null, { search: "" }), true)
})
