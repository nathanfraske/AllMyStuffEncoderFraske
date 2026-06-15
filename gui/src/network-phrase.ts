// Network names as meeting points.
//
// A network id isn't a secret and doesn't gate access (the per-peer auth
// handshake does that) — the signaling rendezvous handle is just a hash of
// the normalized name. So two devices that use the *same name* land on the
// same network. That's the whole idea here: a network "name" is a place you
// agree to meet, not a unique key.
//
// When you don't have a name to meet on, we hand you a memorable one: five
// random words joined by hyphens (e.g. "amber-ladder-quiet-river-seven").
// Not a hash, not a UUID — something you can read aloud or text to someone.
// It's deliberately NOT about uniqueness; a few hundred words is plenty for
// "let's both type this and meet".
//
// Typed names are canonicalized to the daemon's rules (lowercase; letters,
// digits, '-' and '_' only — see `normalize_network_id` in myownmesh-core),
// with spaces folded to hyphens so "Beach House" and "beach-house" meet.

// Short, plain, easy-to-say words. Order/grammar don't matter — these are
// just tokens to combine. Kept ≤8 chars so five of them stay well under the
// 64-char network-id limit.
const WORDS = (
  "amber anchor apple arbor ash aspen autumn azure badger bamboo beacon beaver " +
  "birch bishop bison blossom bonsai boulder brave breeze bridge brook bronze " +
  "button cactus candle canyon cedar cherry cinder clever cliff clover cobalt " +
  "cocoa comet compass copper coral cosmos cottage cougar crimson crystal dahlia " +
  "daisy dapper desert donkey dragon drizzle dune eagle ember emerald falcon " +
  "fennel fern ferret finch fjord forest fox frost garnet gecko gentle geyser " +
  "ginger glacier golden granite gravel grove harbor harvest hazel heron hollow " +
  "honey hornet humble iguana indigo island ivory jade jaguar jasmine jungle " +
  "juniper kettle koala lagoon lantern ladder lemur lilac lizard llama lotus " +
  "magpie mango mantis maple marble maroon marsh marten meadow mellow meteor " +
  "mirror moss moose nebula newt nimble oasis olive onyx orchid otter oyster " +
  "panda parsley pebble pepper pewter pigeon pine plover plum pollen poppy " +
  "prairie puffin quail quartz quiet rabbit raven reef ribbon ridge river robin " +
  "rusty saddle saffron salmon sapphire satchel scarlet shadow shark shore silver " +
  "sleepy snail sparrow spruce stone stork summit sunny sunset swift teal thicket " +
  "thunder tiger timber topaz tulip tundra turtle valley velvet violet walnut " +
  "walrus weasel willow wombat zebra zenith"
).split(" ");

/** A memorable network name: `count` random words joined by hyphens. Valid as
 *  a network id by construction (lowercase, hyphen-separated, well under 64
 *  chars). Not unique by design — it's a shared meeting point. */
export function generateNetworkPhrase(count = 5): string {
  const out: string[] = [];
  for (let i = 0; i < count; i++) {
    out.push(WORDS[Math.floor(Math.random() * WORDS.length)]);
  }
  return out.join("-");
}

/** Fold a typed network name to the canonical id the daemon stores and hashes:
 *  trimmed, lowercased, spaces/underscores → hyphens, anything else dropped,
 *  repeats collapsed. So "Beach House", "beach  house" and "beach-house" all
 *  meet on the same network. May return "" / a too-short string — the caller
 *  validates length (3–64). */
export function canonicalNetworkId(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9-]+/g, "")
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "");
}
