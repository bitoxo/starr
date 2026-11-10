import { createContext, useContext } from 'react'

/** True inside `PrivateArea` (the private category's own area, spec 0007). */
export const InPrivateArea = createContext(false)

export function useInPrivateArea(): boolean {
  return useContext(InPrivateArea)
}
