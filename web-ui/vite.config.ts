import { fileURLToPath, URL } from 'node:url'
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { brotliCompressSync, gzipSync, constants as zlibConstants } from 'node:zlib'
import { defineConfig, type Plugin } from 'vite'
import vue from '@vitejs/plugin-vue'

/**
 * Write `.br` and `.gz` siblings next to every text asset.
 *
 * The service serves the SPA from its own handler (it needs `safe_join`, which
 * is the path-traversal fix), so there is no compression middleware in front of
 * it. Precompressing at build time gives the same result without adding one,
 * and without spending CPU on a broadcast host per request. The originals are
 * kept: the handler falls back to them when the client sends no
 * `Accept-Encoding`.
 *
 * Uses Node's built-in zlib rather than a plugin, so this adds no dependency.
 */
function precompress(): Plugin {
  const COMPRESSIBLE = /\.(js|css|svg|json|html|map)$/
  // Below this, the compressed sibling is not worth the extra request round
  // trip or the disk entry.
  const MIN_BYTES = 1024

  return {
    name: 'playout-precompress',
    apply: 'build',
    enforce: 'post',
    writeBundle(options, bundle) {
      const outDir = options.dir ?? 'dist'
      for (const fileName of Object.keys(bundle)) {
        if (!COMPRESSIBLE.test(fileName)) continue
        const path = join(outDir, fileName)
        let source: Buffer
        try {
          source = readFileSync(path)
        } catch {
          continue
        }
        if (source.length < MIN_BYTES) continue

        writeFileSync(
          `${path}.br`,
          brotliCompressSync(source, {
            params: {
              [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
              [zlibConstants.BROTLI_PARAM_SIZE_HINT]: source.length,
            },
          })
        )
        writeFileSync(`${path}.gz`, gzipSync(source, { level: 9 }))
      }
    },
  }
}

export default defineConfig({
  plugins: [vue(), precompress()],
  base: '/',
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // The UI runs on current Edge or Chrome on the same Windows host as the
    // service, so there is nothing to transpile down for.
    target: 'es2022'
  }
})
