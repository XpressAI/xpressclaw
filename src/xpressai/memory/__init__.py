"""Memory system for XpressAI.

Provides Zettelkasten-style notes with vector search and near-term memory slots.
"""

from xpressai.memory.database import Database
from xpressai.memory.zettelkasten import Memory, Zettelkasten
from xpressai.memory.vector import VectorStore
from xpressai.memory.slots import MemorySlotManager
from xpressai.memory.manager import MemoryManager

__all__ = [
    "Database",
    "Memory",
    "Zettelkasten",
    "VectorStore",
    "MemorySlotManager",
    "MemoryManager",
]
