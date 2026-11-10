// Reports this session's subagents and teammates to the Claude Factory app by rewriting
// ~/.claude-factory/live/<session>.json. The team prompt itself comes from the SessionStart hook in hooks.json.

export function register(on) {
  on('session.start', async ($, e, next) => {
    // The app treats a snapshot older than 10 s as a dead session.
    $.clock.every(2000, () => void snapshot($, false))
    return next(e)
  })

  on('agent.spawn', async ($, e, next) => {
    const result = await next(e)
    await snapshot($, false)
    return result
  }).catch(($, e, next) => next(e)) // Tracking must never block a spawn; next is replay-safe here.

  on('session.end', async ($, e, next) => {
    await snapshot($, true)
    return next(e)
  })
}

// Never throws: in a `-p` run there is no agent list, and tracking is best effort anyway.
async function snapshot($, ended) {
  try {
    const sessionId = await $.session.id()
    const agents = await $.agent.list()
    const body = {
      sessionId,
      cwd: await $.session.cwd(),
      updatedAt: await $.clock.now(),
      ended,
      agents: agents.map(a => ({ id: a.id, name: a.name, type: a.type, description: a.description, status: a.status })),
    }
    const home = await $.env.get('HOME')
    await $.fs.write(home + '/.claude-factory/live/' + sessionId + '.json', JSON.stringify(body))
  } catch {}
}
