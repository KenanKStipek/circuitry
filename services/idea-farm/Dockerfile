# idea-farm — build context is the circuitry repo root
# (Northflank: dockerfile path /services/idea-farm/Dockerfile, context /)
FROM python:3.12-slim

WORKDIR /app

# Install circuitry from the repo at the built SHA — CD on main keeps the
# farm's runtime in lockstep with the library.
COPY pyproject.toml requirements.txt README.md LICENSE ./
COPY src ./src
RUN pip install --no-cache-dir .

COPY services/idea-farm/farm.py services/idea-farm/qa_corpus.py \
     services/idea-farm/idea_generator.yml ./

# State lives here — mount a Northflank volume at /data for durability.
ENV DATA_DIR=/data
VOLUME ["/data"]

CMD ["python", "-u", "farm.py"]
