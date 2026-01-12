#!/usr/bin/env python3
"""Test colorful output"""

print("\n\033[1;36m" + "="*70 + "\033[0m")
print("\033[1;35m   ___        _        ____  _____ _    ")
print("  / _ \\ _   _| |_ ___ |___ \\|  ___/ \\   ")
print(" | | | | | | | __/ _ \\  __) | |_ / _ \\  ")
print(" | |_| | |_| | || (_) |/ __/|  _/ ___ \\ ")
print("  \\___/ \\__,_|\\__\\___/|_____|_|/_/   \\_\\\033[0m")
print()
print("\033[1;37m        Intelligent SSH Auto-Login System\033[0m")
print("\033[1;36m" + "="*70 + "\033[0m")
print("✨ \033[1;32mFeatures:\033[0m Auto-Reconnect | Network Switch | Sleep Recovery")
print("\033[1;36m" + "="*70 + "\033[0m\n")

print("📡 \033[1;34mAvailable Servers:\033[0m")
print("-" * 70)
print("  \033[1;33m[1]\033[0m 🖥️  \033[1;37mserver1.example.com\033[0m")
print("  \033[1;33m[2]\033[0m 🖥️  \033[1;37mserver2.example.com\033[0m")
print("  \033[1;33m[3]\033[0m 🖥️  \033[1;37mserver3.example.com\033[0m")
print("-" * 70)

print("\n\033[1;36m🔑 Connection Process:\033[0m")
print("  \033[1;36m⏳ Attempt 1/5...\033[0m \033[1;32m🔑 Password\033[0m \033[1;35m🔐 2FA Code\033[0m \033[1;32m✅ Connected!\033[0m")

print("\n\033[1;32m✅ Color Test Passed!\033[0m All colors are working beautifully!\n")
