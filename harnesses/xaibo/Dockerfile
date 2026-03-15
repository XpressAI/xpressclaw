FROM ghcr.io/xpressai/xpressclaw-harness-base:latest

WORKDIR /app

# Install Xaibo dependencies
COPY requirements.txt /tmp/xaibo-requirements.txt
RUN pip install --no-cache-dir -r /tmp/xaibo-requirements.txt

COPY harness.py .

# Default agent config mount point
RUN mkdir -p /workspace/agents

EXPOSE 8080
CMD ["python", "harness.py"]
