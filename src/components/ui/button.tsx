import type { ComponentProps } from "react";

type ButtonProps = ComponentProps<"button"> & {
  variant?: "default" | "outline";
  size?: "default" | "sm";
};

export function Button({
  className,
  variant = "default",
  size = "default",
  ...props
}: ButtonProps) {
  const classes = [
    "button",
    variant === "outline" ? "button-outline" : "button-primary",
    size === "sm" && "button-small",
    className,
  ];
  return <button className={classes.filter(Boolean).join(" ")} {...props} />;
}
