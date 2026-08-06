import { Hono } from 'hono'

import { createSiteBuildInfo, type SiteBindings } from './build-info'
import { renderLandingPage } from './page'

const app = new Hono<{ Bindings: SiteBindings }>()

app.get('/', (c) => {
  const buildInfo = createSiteBuildInfo(c.env)
  return c.html(renderLandingPage(buildInfo))
})

export default app
