FROM node:24-bookworm-slim

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev
COPY src/ ./src/
ENV NODE_ENV=production
USER 65532:65532
ENTRYPOINT ["node", "/app/src/main.ts"]
