FROM python:3.11-slim

WORKDIR /app

# Install base dependencies
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY server.py .
COPY mcp_tasks.py .
COPY mcp_apps.py .

EXPOSE 8080

# Subclasses override CMD; this base image isn't meant to be run directly.
CMD ["python", "-c", "print('This is the base image. Use a specific harness image.')"]
