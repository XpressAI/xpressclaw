FROM ghcr.io/xpressai/xpressclaw-harness-base:latest

WORKDIR /app

# Install Claude SDK dependencies
COPY requirements.txt /tmp/claude-requirements.txt
RUN pip install --no-cache-dir -r /tmp/claude-requirements.txt

COPY harness.py .

# Default workspace mount point
RUN mkdir -p /workspace

EXPOSE 8080
CMD ["python", "harness.py"]
