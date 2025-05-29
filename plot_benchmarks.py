import pandas as pd
import matplotlib.pyplot as plt
import seaborn as sns
import numpy as np

# Define the input CSV file and output plot file
CSV_FILE = "benchmark_data.csv"
PLOT_FILE = "benchmark_visualization.png"
PLOT_TITLE = "Codec Performance Comparison (Log Scale for Time)"


def create_visualization():
    """
    Reads benchmark data from a CSV file and creates a visualization
    highlighting rkyv's performance.
    """
    try:
        df = pd.read_csv(CSV_FILE)
    except FileNotFoundError:
        print(
            f"Error: The file {CSV_FILE} was not found. Make sure the benchmarks have been run and the CSV generated."
        )
        return
    except Exception as e:
        print(f"Error reading CSV file: {e}")
        return

    # Convert time to microseconds for better readability if numbers are large, or keep as ns
    # For this dataset, nanoseconds are appropriate.
    # df['time_us'] = df['time_ns'] / 1000

    # Filter out serialize operations for RkyvArchiveStream as it's mainly for access time comparison
    # We keep its 'archive_access' and the 'serialize' for other codecs.
    # Or, better, we can plot serialize and deserialize/access on separate plots or facets.

    # Create a 'Codec Variant' column for clearer legend
    df["codec_variant"] = df["codec_type"] + "_" + df["length_codec"]

    # Separate plots for different operations might be clearer
    operations = df["operation"].unique()

    # Determine the number of rows for subplots
    # We want to give rkyv (especially archive_access) prominence.
    # Let's try:
    # 1. Serialize vs Packet Size
    # 2. Deserialize vs Packet Size
    # 3. Rkyv Archive Access vs Packet Size (and maybe compare to Rkyv Deserialize)

    plt.style.use("seaborn-v0_8_whitegrid")  # Using a seaborn style

    fig, axes = plt.subplots(nrows=3, ncols=1, figsize=(14, 20), sharex=True)
    fig.suptitle(PLOT_TITLE, fontsize=18, fontweight="bold")

    palette = {
        "Rkyv_U32Length": "blue",
        "Rkyv_U64Length": "dodgerblue",
        "Rkyv_VarintLength": "skyblue",
        "RkyvArchiveStream_U32Length": "darkgreen",
        "RkyvArchiveStream_U64Length": "green",
        "RkyvArchiveStream_VarintLength": "limegreen",
        "Bincode_U32Length": "red",
        "Bincode_U64Length": "salmon",
        "Bincode_VarintLength": "lightcoral",
        "SerdeJson_U32Length": "purple",
        "SerdeJson_U64Length": "orchid",
        "SerdeJson_VarintLength": "plum",
    }

    line_styles = {"U32Length": "-", "U64Length": "--", "VarintLength": ":"}

    # --- Plot 1: Serialization Performance ---
    ax1 = axes[0]
    df_ser = df[df["operation"] == "serialize"]
    # Exclude RkyvArchiveStream from general serialization plot as its 'serialize' is just for context to its 'archive_access'
    df_ser_plot = df_ser[df_ser["codec_type"] != "RkyvArchiveStream"]

    for codec_variant in df_ser_plot["codec_variant"].unique():
        subset = df_ser_plot[df_ser_plot["codec_variant"] == codec_variant]
        codec_type, length_codec = codec_variant.split("_", 1)
        sns.lineplot(
            x="packet_size",
            y="time_ns",
            data=subset,
            ax=ax1,
            label=codec_variant,
            color=palette.get(codec_variant),
            linestyle=line_styles.get(length_codec),
            linewidth=2,
        )
    ax1.set_title("Serialization Time vs. Packet Size", fontsize=14)
    ax1.set_ylabel("Time (nanoseconds) - Log Scale", fontsize=12)
    ax1.set_yscale("log")
    ax1.legend(title="Codec (Serialization)", loc="upper left", fontsize="small")
    ax1.tick_params(labelsize=10)

    # --- Plot 2: Deserialization Performance ---
    ax2 = axes[1]
    df_de = df[df["operation"] == "deserialize"]
    for codec_variant in df_de["codec_variant"].unique():
        subset = df_de[df_de["codec_variant"] == codec_variant]
        codec_type, length_codec = codec_variant.split("_", 1)
        sns.lineplot(
            x="packet_size",
            y="time_ns",
            data=subset,
            ax=ax2,
            label=codec_variant,
            color=palette.get(codec_variant),
            linestyle=line_styles.get(length_codec),
            linewidth=2,
        )
    ax2.set_title("Deserialization Time vs. Packet Size", fontsize=14)
    ax2.set_ylabel("Time (nanoseconds) - Log Scale", fontsize=12)
    ax2.set_yscale("log")
    ax2.legend(title="Codec (Deserialization)", loc="upper left", fontsize="small")
    ax2.tick_params(labelsize=10)

    # --- Plot 3: Rkyv Archive Access vs. Rkyv Deserialize ---
    ax3 = axes[2]
    df_rkyv_access = df[
        (df["operation"] == "archive_access")
        & (df["codec_type"] == "RkyvArchiveStream")
    ]
    df_rkyv_de = df[(df["operation"] == "deserialize") & (df["codec_type"] == "Rkyv")]

    combined_rkyv_focus = pd.concat([df_rkyv_access, df_rkyv_de])

    for codec_variant in combined_rkyv_focus["codec_variant"].unique():
        subset = combined_rkyv_focus[
            combined_rkyv_focus["codec_variant"] == codec_variant
        ]
        op_label = (
            "Archive Access" if "ArchiveStream" in codec_variant else "Full Deserialize"
        )
        label_text = (
            f"{codec_variant.replace('RkyvArchiveStream', 'Rkyv')} ({op_label})"
        )
        codec_type, length_codec = codec_variant.split("_", 1)

        sns.lineplot(
            x="packet_size",
            y="time_ns",
            data=subset,
            ax=ax3,
            label=label_text,
            color=palette.get(codec_variant),
            linestyle=line_styles.get(length_codec),
            linewidth=2.5,  # Thicker lines for emphasis
        )

    ax3.set_title(
        "Rkyv: Archive Access vs. Full Deserialization", fontsize=14, fontweight="bold"
    )
    ax3.set_xlabel("Packet Size (bytes)", fontsize=12)
    ax3.set_ylabel("Time (nanoseconds) - Log Scale", fontsize=12)
    ax3.set_yscale("log")
    ax3.legend(title="Rkyv Operation", loc="upper left", fontsize="small")
    ax3.tick_params(labelsize=10)

    # Common X-axis label
    plt.xlabel("Packet Size (bytes)", fontsize=12)

    # Improve layout
    plt.tight_layout(rect=[0, 0, 1, 0.96])  # Adjust rect to make space for suptitle

    # Save the plot
    try:
        plt.savefig(PLOT_FILE, dpi=300)
        print(f"Visualization saved to {PLOT_FILE}")
    except Exception as e:
        print(f"Error saving plot: {e}")


if __name__ == "__main__":
    create_visualization()
