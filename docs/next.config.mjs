/** @type {import('next').NextConfig} */
export default {
  output: 'export',
  basePath: process.env.DOCS_BASE_PATH || '',
  trailingSlash: true,
  images: { unoptimized: true },
}
