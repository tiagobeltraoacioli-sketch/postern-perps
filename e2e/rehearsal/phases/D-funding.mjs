// Phase D: funding over several intervals. Not built yet: recorded as blocked, never as passed. UNAUDITED prototype.
export default async function (ctx) {
  await ctx.blocked('D-01', "funding over several intervals", "the dev genesis keeps the 1 h funding interval and the node has no API for funding payments on main f0e3967");
}
