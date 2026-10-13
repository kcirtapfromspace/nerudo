"""Accounts hold an integer cents balance."""


class InsufficientFunds(Exception):
    """Raised when an account cannot cover a debit."""


class Account:
    def __init__(self, name: str, balance: int = 0):
        if balance < 0:
            raise ValueError("opening balance must be >= 0")
        self.name = name
        self.balance = balance

    def __repr__(self) -> str:
        return f"Account({self.name!r}, balance={self.balance})"
