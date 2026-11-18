// Phase H: a censored user exits by the forced path. Not built yet: recorded as blocked, never as passed. UNAUDITED prototype.
export default async function (ctx) {
  await ctx.blocked('H-01', "a censored user exits by the forced path", "needs the vault on anvil (requestForced) watched by the node");
}
