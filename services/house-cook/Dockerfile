# house-cook — an always-on CyberDiner provider (cookd) for the cheap tier.
# Build context: circuitry repo root; dockerfile /services/house-cook/Dockerfile
# CPU-only (standard variant) — slow per job, but never sleeps.
FROM ubuntu:24.04

RUN apt-get update \
 && apt-get install -y --no-install-recommends curl jq ca-certificates bash \
 && rm -rf /var/lib/apt/lists/*

# Install the latest released cookd binary (public releases repo).
RUN curl -fsSL https://cyberdiner.app/install.sh | bash
ENV PATH="/root/.local/bin:${PATH}"

# Serve the cheap tier via the WS gateway (default transport when PULSAR_URL
# is unset). EXPO_URL can stay default (api.cyberdiner.app) or be overridden
# to the project-internal http://expo:3000 in service env.
ENV TIER_SUBSCRIPTIONS=cheap

# Credential + model cache must survive restarts — mount a volume at /root.
VOLUME ["/root"]

COPY services/house-cook/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
CMD ["/entrypoint.sh"]
