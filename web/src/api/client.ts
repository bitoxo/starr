import createClient from 'openapi-fetch'

import type { paths } from './schema'

/** Typed API client. Types come from the Rust OpenAPI spec: run `just gen-api` after API changes. */
export const api = createClient<paths>({ baseUrl: '/' })
